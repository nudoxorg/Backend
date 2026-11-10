//! Syntactic outline of a SQLite source file (protocol v4).
//!
//! The outline is built from syntaqlite's typed AST and its token stream. It states what a
//! file *writes*: which objects each statement defines or alters, which names it mentions
//! (tables, columns, functions, indexes), and which names are visible where (scope entries).
//! It never binds a column to a table; the owner does that across files.
//!
//! `TEMP`/`TEMPORARY` tables, views and triggers are recorded with schema `temp` unless a schema
//! was written. `parent_schema` is only set from a written qualifier.
//!
//! # Where extents come from
//!
//! The typed AST hands back names as `&str` slices of the source, stripped of quotes. A
//! slice's byte offset is recovered from its address, then matched to the token that
//! contains it. The token gives the exact written spelling (quotes included), which is also
//! what decides the normalized name: ASCII-lowercased, enclosing quotes/brackets removed and
//! doubled-quote escapes undone. An extent is the written token; `full_extent` runs from the
//! first qualifier token to the name token. Node extents (statement, column definition, query
//! block) come from the parser's node-extent tracking. A name whose slice does not line up
//! with one token exactly is dropped and the statement is flagged `Partial`.
//!
//! # Roles
//!
//! * `Read`: SELECT lists, WHERE, JOIN ON, GROUP BY, ORDER BY, FROM, RETURNING, subqueries and
//!   trigger bodies/WHEN. `Write`: INSERT/UPDATE/DELETE target relations, INSERT column lists,
//!   UPDATE SET targets. `Schema`: everything inside DDL that is a dependency: foreign keys,
//!   PRIMARY KEY/UNIQUE lists, CHECK/DEFAULT/GENERATED expressions, index keys and partial
//!   index WHERE, the table a trigger or index is on, ALTER/DROP targets, and the *entire body
//!   of a CREATE VIEW* (relations, columns and functions). `CREATE TABLE ... AS SELECT` bodies
//!   are `Read` (a one-time copy, not a standing dependency).
//!
//! # Synthesized qualifiers
//!
//! A column that the syntax binds to one specific table is emitted with `qualifier` set to
//! that table although it was not written there: PRIMARY KEY/UNIQUE/FOREIGN KEY lists and the
//! referenced columns of a REFERENCES clause, index key columns, `UPDATE OF` columns in
//! triggers, INSERT column lists, ON CONFLICT targets, and UPDATE SET targets (the target's
//! alias if it has one). `full_extent` is then just the name token. Every other column keeps
//! exactly what was written: `a.b` gives qualifier `a`, a bare `b` gives `None`.
//!
//! # Not column references
//!
//! Result-column aliases, CTE names, window names and parameters (`?1`, `:x`) are never column
//! references. An unqualified name that equals a result-column alias of the same query block is
//! not emitted when it appears in WHERE, GROUP BY, HAVING or ORDER BY (SQLite resolves such a
//! name to the alias in ORDER BY and, when no FROM column has that name, elsewhere). The cost
//! is that a real column of the same name used there is not listed; the select list nearly
//! always mentions it too. Bare names in a compound SELECT's ORDER BY name output columns and
//! are skipped. A bare name in a DEFAULT expression cannot be a column in SQLite and is skipped.
//!
//! # Double-quoted identifiers (DQS)
//!
//! SQLite treats `"x"` in an expression as a column if one resolves and as a string literal
//! otherwise. Syntax alone cannot tell, so a bare double-quoted name in expression position is
//! emitted as an unqualified Column reference, and its extent starts with `"` in the source.
//! The owner applies the string fallback when no column resolves. Qualified (`t."x"`) and
//! declaration positions are never strings. Single-quoted text is a literal and is not emitted.
//!
//! # Gaps
//!
//! `Partial(gap)` is set whenever something was skipped. See the crate README for the list.
use backend_sql_contract::{
    Definition, DefinitionAction, Extent, Gap, MAX_OUTLINE_ITEMS, MAX_OUTLINE_STATEMENTS,
    MAX_OUTLINE_TEXT, ObjectKind, Outline, OutlineCoverage, RefKind, RefRole, Reference,
    ScopeEntry, ScopeKind, StatementKind, StatementOutline, StatementSupport,
};
use syntaqlite::any::AnyNodeId;
use syntaqlite::nodes as n;
use syntaqlite::parse::ParsedStatement;

/// Deepest nesting of expressions and query blocks the walker follows.
const MAX_DEPTH: usize = 192;

/// A normalized name and the written token it came from.
#[derive(Clone)]
struct Nm {
    norm: String,
    ext: Extent,
}

/// What an implicitly bound column is qualified with.
#[derive(Clone)]
struct Qual {
    schema: Option<String>,
    name: String,
}

#[derive(Clone, Copy)]
struct Ctx<'c> {
    role: RefRole,
    /// Result-column aliases of the current query block; unqualified matches are skipped.
    aliases: Option<&'c [String]>,
    /// Skip every unqualified column (compound ORDER BY names output columns).
    skip_bare: bool,
    /// Column references cannot occur here (DEFAULT).
    no_columns: bool,
}

impl<'c> Ctx<'c> {
    fn new(role: RefRole) -> Self {
        Self {
            role,
            aliases: None,
            skip_bare: false,
            no_columns: false,
        }
    }
    fn with_aliases(self, aliases: &'c [String]) -> Self {
        Self {
            aliases: Some(aliases),
            ..self
        }
    }
}

/// Accumulates statement outlines for one document.
pub(crate) struct Builder {
    statements: Vec<StatementOutline>,
    remaining_items: usize,
    truncated: bool,
}

impl Builder {
    pub(crate) fn new() -> Self {
        Self {
            statements: Vec::new(),
            remaining_items: MAX_OUTLINE_ITEMS,
            truncated: false,
        }
    }

    /// Add one parsed statement. `Err` means the parser's coordinates were internally
    /// inconsistent, which makes the whole outline untrustworthy.
    pub(crate) fn add<'a>(&mut self, stmt: &'a ParsedStatement<'a>) -> Result<(), ()> {
        let Some(root) = stmt.root() else {
            return Ok(()); // comment-only input: no statement
        };
        if self.statements.len() >= MAX_OUTLINE_STATEMENTS {
            self.truncated = true;
            return Ok(());
        }
        let mut cx = Cx::new(stmt, self.remaining_items)?;
        if self.remaining_items == 0 {
            // The file's item budget is spent: record the statement, extract nothing.
            self.statements.push(StatementOutline {
                kind: StatementKind::Other,
                extent: cx.stmt_extent.clone(),
                support: StatementSupport::Unsupported(Gap::Limit),
                definitions: Vec::new(),
                references: Vec::new(),
                scope: Vec::new(),
            });
            return Ok(());
        }
        let kind = cx.statement(root);
        let used = cx.used;
        let support = match cx.gap {
            None => StatementSupport::Full,
            Some(gap) => StatementSupport::Partial(gap),
        };
        let out = StatementOutline {
            kind,
            extent: cx.stmt_extent.clone(),
            support,
            definitions: std::mem::take(&mut cx.defs),
            references: {
                let mut refs = std::mem::take(&mut cx.refs);
                refs.sort_by_key(|r| (r.full_extent.start, r.extent.start, r.extent.end));
                refs
            },
            scope: std::mem::take(&mut cx.scope),
        };
        self.remaining_items = self.remaining_items.saturating_sub(used);
        self.statements.push(out);
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Outline {
        if self.truncated {
            // More statements than the contract carries: say so on the last one kept.
            if let Some(last) = self.statements.last_mut() {
                if matches!(last.support, StatementSupport::Full) {
                    last.support = StatementSupport::Partial(Gap::Limit);
                }
            }
        }
        let all_full = self
            .statements
            .iter()
            .all(|s| matches!(s.support, StatementSupport::Full));
        Outline {
            coverage: if all_full {
                OutlineCoverage::Complete
            } else {
                OutlineCoverage::Partial
            },
            statements: self.statements,
        }
    }
}

struct Cx<'a> {
    stmt: &'a ParsedStatement<'a>,
    src: &'a str,
    base: usize,
    /// Document byte ranges of the statement's tokens, ascending.
    tokens: Vec<(usize, usize)>,
    stmt_extent: Extent,
    /// Region of the DML/DDL statement being walked (a trigger body statement nests).
    block: Extent,
    defs: Vec<Definition>,
    refs: Vec<Reference>,
    scope: Vec<ScopeEntry>,
    gap: Option<Gap>,
    used: usize,
    limit: usize,
    depth: usize,
}

fn ext(start: usize, end: usize) -> Extent {
    Extent { start, end }
}

fn normalize(inner: &str, quote: Option<u8>) -> String {
    let mut out = String::with_capacity(inner.len());
    match quote {
        Some(q @ (b'"' | b'`' | b'\'')) => {
            let q = q as char;
            let mut chars = inner.chars().peekable();
            while let Some(c) = chars.next() {
                out.push(c);
                if c == q && chars.peek() == Some(&q) {
                    chars.next();
                }
            }
        }
        _ => out.push_str(inner),
    }
    out.make_ascii_lowercase();
    out
}

impl<'a> Cx<'a> {
    fn new(stmt: &'a ParsedStatement<'a>, limit: usize) -> Result<Self, ()> {
        let src = stmt.full_text().as_str();
        let base = stmt.statement_base();
        let base = base_to_usize(stmt, base);
        let mut tokens = Vec::new();
        for t in stmt.tokens() {
            let start = base + t.offset().as_usize();
            let end = start + t.length().as_usize();
            if src.get(start..end) != Some(t.text()) || start >= end {
                return Err(());
            }
            if tokens.last().is_some_and(|&(_, e)| start < e) {
                return Err(());
            }
            tokens.push((start, end));
        }
        let mut cx = Cx {
            stmt,
            src,
            base,
            tokens,
            stmt_extent: ext(0, 0),
            block: ext(0, 0),
            defs: Vec::new(),
            refs: Vec::new(),
            scope: Vec::new(),
            gap: None,
            used: 0,
            limit,
            depth: 0,
        };
        let root = stmt.root().ok_or(())?;
        let mut extent = cx.extent_of(root.node_id());
        if extent.is_none() {
            // Fall back to the token run, without the trailing semicolon.
            let mut toks = cx.tokens.iter().filter(|t| &src[t.0..t.1] != ";");
            if let (Some(first), Some(last)) = (toks.next(), toks.last().or(cx.tokens.first())) {
                extent = Some(ext(first.0, last.1));
            }
        }
        let extent = extent.ok_or(())?;
        cx.block = extent.clone();
        cx.stmt_extent = extent;
        if limit == 0 {
            cx.gap = Some(Gap::Limit);
        }
        Ok(cx)
    }

    // ---- bookkeeping ---------------------------------------------------------------------

    fn gap(&mut self, gap: Gap) {
        self.gap.get_or_insert(gap);
    }

    fn room(&mut self) -> bool {
        if self.used >= self.limit {
            self.gap(Gap::Limit);
            false
        } else {
            self.used += 1;
            true
        }
    }

    fn extent_of(&self, id: impl Into<AnyNodeId>) -> Option<Extent> {
        let (text, off) = self.stmt.node_text(id.into())?;
        let start = self.base + off.as_usize();
        let end = start + text.len();
        (start < end && self.src.get(start..end) == Some(text)).then(|| ext(start, end))
    }

    fn extent_or_block(&self, id: impl Into<AnyNodeId>) -> Extent {
        self.extent_of(id).unwrap_or_else(|| self.block.clone())
    }

    // ---- names ---------------------------------------------------------------------------

    fn decode_token(&mut self, index: usize) -> Option<Nm> {
        let (start, end) = self.tokens[index];
        let text = &self.src[start..end];
        let first = text.as_bytes()[0];
        let (inner, quote) = match first {
            b'"' | b'`' | b'\'' | b'[' => {
                let closer = if first == b'[' { b']' } else { first };
                if text.len() < 2 || text.as_bytes()[text.len() - 1] != closer {
                    self.gap(Gap::UnmodeledConstruct);
                    return None;
                }
                (&text[1..text.len() - 1], Some(first))
            }
            _ => (text, None),
        };
        let norm = normalize(inner, quote);
        if norm.is_empty() || norm.len() > MAX_OUTLINE_TEXT || norm.contains('\0') {
            self.gap(if norm.is_empty() {
                Gap::UnmodeledConstruct
            } else {
                Gap::Limit
            });
            return None;
        }
        Some(Nm {
            norm,
            ext: ext(start, end),
        })
    }

    fn token_containing(&self, offset: usize) -> Option<usize> {
        let index = self.tokens.partition_point(|t| t.1 <= offset);
        (index < self.tokens.len() && self.tokens[index].0 <= offset).then_some(index)
    }

    /// Resolve a name slice from the typed AST to its written token. Absent (empty) names
    /// give `None` without a gap; a slice that does not match one token gives `None` and a gap.
    fn name(&mut self, s: &str) -> Option<Nm> {
        if s.is_empty() {
            return None;
        }
        let Some(offset) = (s.as_ptr() as usize).checked_sub(self.src.as_ptr() as usize) else {
            self.gap(Gap::UnmodeledConstruct);
            return None;
        };
        let end = offset + s.len();
        let Some(index) = self
            .token_containing(offset)
            .filter(|_| end <= self.src.len())
        else {
            self.gap(Gap::UnmodeledConstruct);
            return None;
        };
        let (tstart, tend) = self.tokens[index];
        let quoted = matches!(self.src.as_bytes()[tstart], b'"' | b'`' | b'\'' | b'[');
        let (istart, iend) = if quoted {
            (tstart + 1, tend - 1)
        } else {
            (tstart, tend)
        };
        if (istart, iend) != (offset, end) {
            self.gap(Gap::UnmodeledConstruct);
            return None;
        }
        self.decode_token(index)
    }

    fn ident(&mut self, name: Option<n::Name<'a>>) -> Option<Nm> {
        match name? {
            n::Name::IdentName(i) => self.name(i.source()),
            n::Name::Error(_) => {
                self.gap(Gap::UnmodeledConstruct);
                None
            }
        }
    }

    // ---- emission ------------------------------------------------------------------------

    fn push_ref(
        &mut self,
        kind: RefKind,
        role: RefRole,
        schema: Option<&Nm>,
        qualifier: Option<String>,
        name: &Nm,
        first: Option<&Nm>,
    ) {
        if !self.room() {
            return;
        }
        let start = first
            .map(|f| f.ext.start)
            .into_iter()
            .chain(schema.map(|s| s.ext.start))
            .min()
            .unwrap_or(name.ext.start)
            .min(name.ext.start);
        self.refs.push(Reference {
            kind,
            role,
            schema: schema.map(|s| s.norm.clone()),
            qualifier,
            name: name.norm.clone(),
            extent: name.ext.clone(),
            full_extent: ext(start, name.ext.end),
        });
    }

    fn add_scope(&mut self, entry: ScopeEntry) {
        if self.room() {
            self.scope.push(entry);
        }
    }

    fn add_def(&mut self, def: Definition) {
        if self.room() {
            self.defs.push(def);
        }
    }

    fn detail(&mut self, text: &str) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        if text.len() > MAX_OUTLINE_TEXT || text.contains('\0') {
            self.gap(Gap::Limit);
            return None;
        }
        Some(text.to_owned())
    }

    /// A relation in a relation position (table, view or CTE name).
    fn relation_ref(&mut self, role: RefRole, schema: &str, table: &str) -> Option<(Option<Nm>, Nm)> {
        let schema = self.name(schema);
        let table = self.name(table)?;
        self.push_ref(RefKind::Relation, role, schema.as_ref(), None, &table, None);
        Some((schema, table))
    }

    fn function_ref(&mut self, name: &str, role: RefRole) {
        if let Some(name) = self.name(name) {
            self.push_ref(RefKind::Function, role, None, None, &name, None);
        }
    }

    fn column_ref(&mut self, c: n::ColumnRef<'a>, ctx: Ctx<'_>, synth: Option<&Qual>) {
        if ctx.no_columns {
            return;
        }
        let Some(column) = self.name(c.column()) else {
            return;
        };
        let table = self.name(c.table());
        let schema = self.name(c.schema());
        if table.is_none() && synth.is_none() {
            if ctx.skip_bare {
                return;
            }
            if ctx.aliases.is_some_and(|a| a.contains(&column.norm)) {
                return;
            }
        }
        match (&table, synth) {
            (Some(t), _) => {
                self.push_ref(
                    RefKind::Column,
                    ctx.role,
                    schema.as_ref(),
                    Some(t.norm.clone()),
                    &column,
                    Some(t),
                );
            }
            (None, Some(q)) => {
                if !self.room() {
                    return;
                }
                self.refs.push(Reference {
                    kind: RefKind::Column,
                    role: ctx.role,
                    schema: q.schema.clone(),
                    qualifier: Some(q.name.clone()),
                    name: column.norm.clone(),
                    extent: column.ext.clone(),
                    full_extent: column.ext.clone(),
                });
            }
            (None, None) => {
                self.push_ref(RefKind::Column, ctx.role, None, None, &column, None);
            }
        }
    }

    // ---- expressions ---------------------------------------------------------------------

    fn expr(&mut self, e: Option<n::Expr<'a>>, ctx: Ctx<'_>) {
        let Some(e) = e else { return };
        if self.depth >= MAX_DEPTH {
            self.gap(Gap::Limit);
            return;
        }
        self.depth += 1;
        self.expr_inner(e, ctx);
        self.depth -= 1;
    }

    fn expr_list(&mut self, list: Option<n::ExprList<'a>>, ctx: Ctx<'_>) {
        if let Some(list) = list {
            for e in list.iter() {
                self.expr(Some(e), ctx);
            }
        }
    }

    fn expr_inner(&mut self, e: n::Expr<'a>, ctx: Ctx<'_>) {
        use n::Expr as E;
        match e {
            E::BinaryExpr(b) => {
                self.expr(b.left(), ctx);
                self.expr(b.right(), ctx);
            }
            E::UnaryExpr(u) => self.expr(u.operand(), ctx),
            E::ParenExpr(p) => self.expr(p.expr(), ctx),
            E::Literal(_) | E::Variable(_) => {}
            E::ColumnRef(c) => self.column_ref(c, ctx, None),
            E::Error(_) => self.gap(Gap::UnmodeledConstruct),
            E::FunctionCall(f) => {
                self.function_ref(f.func_name(), ctx.role);
                self.expr_list(f.args(), ctx);
                self.expr(f.filter_clause(), ctx);
                self.window(f.over_clause(), ctx);
            }
            E::AggregateFunctionCall(f) => {
                self.function_ref(f.func_name(), ctx.role);
                self.expr_list(f.args(), ctx);
                self.order_terms(f.orderby(), ctx);
                self.expr(f.filter_clause(), ctx);
                self.window(f.over_clause(), ctx);
            }
            E::OrderedSetFunctionCall(f) => {
                self.function_ref(f.func_name(), ctx.role);
                self.expr_list(f.args(), ctx);
                self.expr(f.orderby_expr(), ctx);
                self.expr(f.filter_clause(), ctx);
                self.window(f.over_clause(), ctx);
            }
            E::CastExpr(c) => self.expr(c.expr(), ctx),
            E::CollateExpr(c) => self.expr(c.expr(), ctx),
            E::CaseExpr(c) => {
                self.expr(c.operand(), ctx);
                if let Some(whens) = c.whens() {
                    for w in whens.iter() {
                        self.expr(w.when_expr(), ctx);
                        self.expr(w.then_expr(), ctx);
                    }
                }
                self.expr(c.else_expr(), ctx);
            }
            E::IsExpr(i) => {
                self.expr(i.left(), ctx);
                self.expr(i.right(), ctx);
            }
            E::BetweenExpr(b) => {
                self.expr(b.operand(), ctx);
                self.expr(b.low(), ctx);
                self.expr(b.high(), ctx);
            }
            E::LikeExpr(l) => {
                self.expr(l.operand(), ctx);
                self.expr(l.pattern(), ctx);
                self.expr(l.escape(), ctx);
            }
            E::InExpr(i) => {
                self.expr(i.operand(), ctx);
                use n::InExprSource as S;
                match i.source() {
                    None => {}
                    Some(S::ExprList(list)) => self.expr_list(Some(list), ctx),
                    Some(S::SubqueryExpr(s)) => self.select(s.select(), ctx.role),
                    Some(S::SelectStmt(s)) => self.select_core(s, ctx.role),
                    Some(S::CompoundSelect(s)) => self.compound(s, ctx.role),
                    Some(S::WithClause(s)) => self.with_clause(s, ctx.role),
                    Some(S::ValuesClause(s)) => self.values_clause(s, ctx.role),
                    Some(S::TableRef(t)) => {
                        let _ = self.relation_ref(ctx.role, t.schema(), t.table_name());
                    }
                }
            }
            E::SubqueryExpr(s) => self.select(s.select(), ctx.role),
            E::ExistsExpr(s) => self.select(s.select(), ctx.role),
            E::RaiseExpr(r) => self.expr(r.error_message(), ctx),
            E::RowValue(r) => self.expr_list(r.items(), ctx),
        }
    }

    fn order_terms(&mut self, list: Option<n::OrderByList<'a>>, ctx: Ctx<'_>) {
        if let Some(list) = list {
            for term in list.iter() {
                self.expr(term.expr(), ctx);
            }
        }
    }

    fn window(&mut self, w: Option<n::WindowDef<'a>>, ctx: Ctx<'_>) {
        let Some(w) = w else { return };
        self.expr_list(w.partition_by(), ctx);
        self.order_terms(w.orderby(), ctx);
        if let Some(frame) = w.frame() {
            for bound in [frame.start_bound(), frame.end_bound()].into_iter().flatten() {
                self.expr(bound.expr(), ctx);
            }
        }
    }

    // ---- queries -------------------------------------------------------------------------

    fn select(&mut self, s: Option<n::Select<'a>>, role: RefRole) {
        match s {
            None => {}
            Some(n::Select::SelectStmt(s)) => self.select_core(s, role),
            Some(n::Select::CompoundSelect(s)) => self.compound(s, role),
            Some(n::Select::WithClause(s)) => self.with_clause(s, role),
            Some(n::Select::ValuesClause(s)) => self.values_clause(s, role),
        }
    }

    fn nested<F: FnOnce(&mut Self)>(&mut self, f: F) {
        if self.depth >= MAX_DEPTH {
            self.gap(Gap::Limit);
            return;
        }
        self.depth += 1;
        f(self);
        self.depth -= 1;
    }

    fn values_clause(&mut self, v: n::ValuesClause<'a>, role: RefRole) {
        self.nested(|cx| {
            if let Some(rows) = v.rows() {
                for row in rows.iter() {
                    cx.expr_list(Some(row), Ctx::new(role));
                }
            }
        });
    }

    fn compound(&mut self, c: n::CompoundSelect<'a>, role: RefRole) {
        self.nested(|cx| {
            cx.select(c.left(), role);
            cx.select(c.right(), role);
            let ctx = Ctx {
                skip_bare: true,
                ..Ctx::new(role)
            };
            cx.order_terms(c.orderby(), ctx);
            cx.limit(c.limit_clause(), Ctx::new(role));
        });
    }

    fn limit(&mut self, l: Option<n::LimitClause<'a>>, ctx: Ctx<'_>) {
        if let Some(l) = l {
            self.expr(l.limit(), ctx);
            self.expr(l.offset(), ctx);
        }
    }

    fn ctes(&mut self, list: Option<n::CteList<'a>>, region: &Extent, role: RefRole) {
        let Some(list) = list else { return };
        for cte in list.iter() {
            if let Some(name) = self.name(cte.cte_name()) {
                let columns = cte.columns().map(|cols| self.column_names(cols));
                self.add_scope(ScopeEntry {
                    kind: ScopeKind::Cte,
                    visible_as: name.norm,
                    relation: None,
                    relation_schema: None,
                    columns,
                    region: region.clone(),
                });
            }
            self.select(cte.select(), role);
        }
    }

    /// Normalized names of an explicit column list; unresolvable entries set a gap.
    fn column_names(&mut self, list: n::ExprList<'a>) -> Vec<String> {
        let mut out = Vec::new();
        for item in list.iter() {
            match item {
                n::Expr::ColumnRef(c) => {
                    if let Some(nm) = self.name(c.column()) {
                        out.push(nm.norm);
                    }
                }
                _ => self.gap(Gap::UnmodeledConstruct),
            }
        }
        out
    }

    fn with_clause(&mut self, w: n::WithClause<'a>, role: RefRole) {
        self.nested(|cx| {
            let region = cx.extent_or_block(w.node_id());
            cx.ctes(w.ctes(), &region, role);
            cx.select(w.select(), role);
        });
    }

    fn select_core(&mut self, s: n::SelectStmt<'a>, role: RefRole) {
        self.nested(|cx| {
            let region = cx.extent_or_block(s.node_id());
            let mut aliases = Vec::new();
            if let Some(cols) = s.columns() {
                for rc in cols.iter() {
                    if let Some(a) = rc.alias() {
                        if let Some(nm) = cx.ident(Some(a)) {
                            aliases.push(nm.norm);
                        }
                    }
                }
            }
            if let Some(from) = s.from_clause() {
                cx.table_source(from, &region, role);
            }
            if let Some(cols) = s.columns() {
                cx.result_columns(cols, Ctx::new(role));
            }
            let ctx = Ctx::new(role).with_aliases(&aliases);
            cx.expr(s.where_clause(), ctx);
            cx.expr_list(s.groupby(), ctx);
            cx.expr(s.having(), ctx);
            cx.order_terms(s.orderby(), ctx);
            cx.limit(s.limit_clause(), Ctx::new(role));
            if let Some(windows) = s.window_clause() {
                for w in windows.iter() {
                    cx.window(w.window_def(), Ctx::new(role));
                }
            }
        });
    }

    fn result_columns(&mut self, list: n::ResultColumnList<'a>, ctx: Ctx<'_>) {
        for rc in list.iter() {
            if rc.flags().star() {
                self.star(&rc, ctx);
            } else {
                self.expr(rc.expr(), ctx);
            }
        }
    }

    /// `*` or `t.*`. The typed AST drops the qualifier, so it is read off the tokens. A list
    /// item's node extent may start at the head of the list, so only its end is trusted and
    /// the qualifier is found by walking back from the `*` token.
    fn star(&mut self, rc: &n::ResultColumn<'a>, ctx: Ctx<'_>) {
        let end = self.extent_of(rc.node_id()).map(|e| e.end);
        let star = end.and_then(|end| {
            let i = self.tokens.partition_point(|t| t.1 < end);
            (i < self.tokens.len() && self.tokens[i].1 == end && &self.src[self.tokens[i].0..end] == "*")
                .then_some(i)
        });
        let Some(star) = star else {
            self.gap(Gap::UnmodeledConstruct);
            return;
        };
        let is_dot = |cx: &Self, i: usize| &cx.src[cx.tokens[i].0..cx.tokens[i].1] == ".";
        let star_ext = ext(self.tokens[star].0, self.tokens[star].1);
        let mut qualifier = None;
        let mut schema = None;
        if star >= 2 && is_dot(self, star - 1) {
            qualifier = self.decode_token(star - 2);
            if qualifier.is_none() {
                return;
            }
            if star >= 4 && is_dot(self, star - 3) {
                schema = self.decode_token(star - 4);
                if schema.is_none() {
                    return;
                }
            }
        }
        if !self.room() {
            return;
        }
        let start = schema
            .as_ref()
            .or(qualifier.as_ref())
            .map_or(star_ext.start, |q| q.ext.start);
        self.refs.push(Reference {
            kind: RefKind::Column,
            role: ctx.role,
            schema: schema.map(|s| s.norm),
            qualifier: qualifier.map(|q| q.norm),
            name: "*".to_owned(),
            extent: star_ext.clone(),
            full_extent: ext(start, star_ext.end),
        });
    }

    /// FROM items. Every item makes its visible name valid inside `region`.
    fn table_source(&mut self, ts: n::TableSource<'a>, region: &Extent, role: RefRole) {
        if self.depth >= MAX_DEPTH {
            self.gap(Gap::Limit);
            return;
        }
        self.depth += 1;
        match ts {
            n::TableSource::TableRef(t) => self.table_ref_item(t, region, role),
            n::TableSource::SubqueryTableSource(s) => {
                self.select(s.select(), role);
                if let Some(alias) = self.ident(s.alias()) {
                    self.add_scope(ScopeEntry {
                        kind: ScopeKind::Subquery,
                        visible_as: alias.norm,
                        relation: None,
                        relation_schema: None,
                        columns: None,
                        region: region.clone(),
                    });
                }
            }
            n::TableSource::ParenTableSource(p) => {
                if let Some(inner) = p.source() {
                    self.table_source(inner, region, role);
                }
                if p.alias().is_some() {
                    // An alias on a parenthesised join has no modelled meaning.
                    self.gap(Gap::UnmodeledConstruct);
                }
            }
            n::TableSource::JoinClause(j) => {
                if let Some(left) = j.left() {
                    self.table_source(left, region, role);
                }
                if let Some(right) = j.right() {
                    self.table_source(right, region, role);
                }
                self.expr(j.on_expr(), Ctx::new(role));
                self.expr_list(j.using_columns(), Ctx::new(role));
            }
            n::TableSource::JoinPrefix(j) => {
                if let Some(source) = j.source() {
                    self.table_source(source, region, role);
                }
            }
        }
        self.depth -= 1;
    }

    fn table_ref_item(&mut self, t: n::TableRef<'a>, region: &Extent, role: RefRole) {
        let alias = self.ident(t.alias());
        if let Some(args) = t.args() {
            // Table-valued function in FROM.
            if let Some(name) = self.name(t.table_name()) {
                self.push_ref(RefKind::Function, role, None, None, &name, None);
                let visible = alias.as_ref().unwrap_or(&name).norm.clone();
                self.add_scope(ScopeEntry {
                    kind: ScopeKind::Function,
                    visible_as: visible,
                    relation: None,
                    relation_schema: None,
                    columns: None,
                    region: region.clone(),
                });
            }
            self.expr_list(Some(args), Ctx::new(role));
            return;
        }
        if let Some((schema, table)) = self.relation_ref(role, t.schema(), t.table_name()) {
            self.add_scope(ScopeEntry {
                kind: ScopeKind::Relation,
                visible_as: alias.as_ref().unwrap_or(&table).norm.clone(),
                relation: Some(table.norm),
                relation_schema: schema.map(|s| s.norm),
                columns: None,
                region: region.clone(),
            });
        }
        if let Some(index) = self.name(t.index_name()) {
            self.push_ref(RefKind::Index, RefRole::Read, None, None, &index, None);
        }
    }

    // ---- statements ----------------------------------------------------------------------

    fn statement(&mut self, s: n::Stmt<'a>) -> StatementKind {
        use n::Stmt as S;
        if self.depth >= MAX_DEPTH {
            self.gap(Gap::Limit);
            return StatementKind::Other;
        }
        let saved = self.block.clone();
        self.block = self.extent_or_block(s.node_id());
        self.depth += 1;
        let kind = match s {
            S::SelectStmt(x) => {
                self.select_core(x, RefRole::Read);
                StatementKind::Select
            }
            S::CompoundSelect(x) => {
                self.compound(x, RefRole::Read);
                StatementKind::Select
            }
            S::ValuesClause(x) => {
                self.values_clause(x, RefRole::Read);
                StatementKind::Select
            }
            S::WithClause(x) => {
                self.with_clause(x, RefRole::Read);
                StatementKind::Select
            }
            S::InsertStmt(x) => {
                self.insert(x);
                StatementKind::Insert
            }
            S::UpdateStmt(x) => {
                self.update(x);
                StatementKind::Update
            }
            S::DeleteStmt(x) => {
                self.delete(x);
                StatementKind::Delete
            }
            S::CreateTableStmt(x) => {
                self.create_table(x);
                StatementKind::CreateTable
            }
            S::CreateIndexStmt(x) => {
                self.create_index(x);
                StatementKind::CreateIndex
            }
            S::CreateViewStmt(x) => {
                self.create_view(x);
                StatementKind::CreateView
            }
            S::CreateTriggerStmt(x) => {
                self.create_trigger(x);
                StatementKind::CreateTrigger
            }
            S::CreateVirtualTableStmt(x) => {
                self.create_virtual(x);
                StatementKind::CreateTable
            }
            S::DropStmt(x) => {
                self.drop_stmt(x);
                StatementKind::Drop
            }
            S::AlterTableStmt(x) => {
                self.alter(x);
                StatementKind::AlterTable
            }
            S::TransactionStmt(_) | S::SavepointStmt(_) | S::DetachStmt(_) => {
                if matches!(s, S::DetachStmt(_)) {
                    self.gap(Gap::UnmodeledStatement);
                }
                StatementKind::Other
            }
            S::VacuumStmt(_) => StatementKind::Other,
            S::PragmaStmt(p) => {
                const OBJECT_PRAGMAS: &[&str] = &[
                    "table_info",
                    "table_xinfo",
                    "table_list",
                    "index_list",
                    "index_info",
                    "index_xinfo",
                    "foreign_key_list",
                    "foreign_key_check",
                    "integrity_check",
                    "quick_check",
                ];
                if !p.value().is_empty()
                    && OBJECT_PRAGMAS.contains(&p.pragma_name().to_ascii_lowercase().as_str())
                {
                    self.gap(Gap::UnmodeledConstruct);
                }
                StatementKind::Other
            }
            S::AnalyzeOrReindexStmt(a) => {
                if !a.target_name().is_empty() {
                    self.gap(Gap::UnmodeledConstruct);
                }
                StatementKind::Other
            }
            S::AttachStmt(_) | S::ExplainStmt(_) => {
                self.gap(Gap::UnmodeledStatement);
                StatementKind::Other
            }
        };
        self.depth -= 1;
        self.block = saved;
        kind
    }

    // ---- DML -----------------------------------------------------------------------------

    /// Target relation of INSERT/UPDATE/DELETE: a Write reference and a scope entry.
    fn dml_target(&mut self, t: Option<n::TableRef<'a>>) -> Option<(Qual, Nm, Option<Nm>)> {
        let t = t?;
        let alias = self.ident(t.alias());
        let (schema, table) = self.relation_ref(RefRole::Write, t.schema(), t.table_name())?;
        let visible = alias.as_ref().unwrap_or(&table).norm.clone();
        let block = self.block.clone();
        self.add_scope(ScopeEntry {
            kind: ScopeKind::Relation,
            visible_as: visible.clone(),
            relation: Some(table.norm.clone()),
            relation_schema: schema.as_ref().map(|s| s.norm.clone()),
            columns: None,
            region: block,
        });
        if let Some(index) = self.name(t.index_name()) {
            self.push_ref(RefKind::Index, RefRole::Read, None, None, &index, None);
        }
        let qual = Qual {
            schema: if alias.is_some() {
                None
            } else {
                schema.as_ref().map(|s| s.norm.clone())
            },
            name: visible,
        };
        Some((qual, table, schema))
    }

    fn bound_columns(&mut self, list: Option<n::ExprList<'a>>, role: RefRole, qual: Option<&Qual>) {
        let Some(list) = list else { return };
        for e in list.iter() {
            match e {
                n::Expr::ColumnRef(c) => self.column_ref(c, Ctx::new(role), qual),
                other => self.expr(Some(other), Ctx::new(role)),
            }
        }
    }

    fn returning(&mut self, list: Option<n::ResultColumnList<'a>>) {
        if let Some(list) = list {
            self.result_columns(list, Ctx::new(RefRole::Read));
        }
    }

    fn set_clauses(&mut self, list: Option<n::SetClauseList<'a>>, qual: Option<&Qual>) {
        let Some(list) = list else { return };
        for set in list.iter() {
            if let Some(column) = self.name(set.column()) {
                if let Some(q) = qual {
                    if self.room() {
                        self.refs.push(Reference {
                            kind: RefKind::Column,
                            role: RefRole::Write,
                            schema: q.schema.clone(),
                            qualifier: Some(q.name.clone()),
                            name: column.norm.clone(),
                            extent: column.ext.clone(),
                            full_extent: column.ext.clone(),
                        });
                    }
                } else {
                    self.push_ref(RefKind::Column, RefRole::Write, None, None, &column, None);
                }
            }
            self.bound_columns(set.columns(), RefRole::Write, qual);
            self.expr(set.value(), Ctx::new(RefRole::Read));
        }
    }

    fn insert(&mut self, i: n::InsertStmt<'a>) {
        let block = self.block.clone();
        self.ctes(i.with_ctes(), &block, RefRole::Read);
        let target = self.dml_target(i.table());
        let qual = target.as_ref().map(|t| &t.0);
        self.bound_columns(i.columns(), RefRole::Write, qual);
        self.select(i.source(), RefRole::Read);
        if let Some(upserts) = i.upsert() {
            for u in upserts.iter() {
                let region = self.extent_or_block(u.node_id());
                if let Some((_, table, schema)) = &target {
                    self.add_scope(ScopeEntry {
                        kind: ScopeKind::Relation,
                        visible_as: "excluded".to_owned(),
                        relation: Some(table.norm.clone()),
                        relation_schema: schema.as_ref().map(|s| s.norm.clone()),
                        columns: None,
                        region,
                    });
                }
                if let Some(terms) = u.columns() {
                    for term in terms.iter() {
                        match term.expr() {
                            Some(n::Expr::ColumnRef(c)) => {
                                self.column_ref(c, Ctx::new(RefRole::Read), qual);
                            }
                            other => self.expr(other, Ctx::new(RefRole::Read)),
                        }
                    }
                }
                self.expr(u.target_where(), Ctx::new(RefRole::Read));
                self.set_clauses(u.setlist(), qual);
                self.expr(u.update_where(), Ctx::new(RefRole::Read));
            }
        }
        self.returning(i.returning());
    }

    fn update(&mut self, u: n::UpdateStmt<'a>) {
        let block = self.block.clone();
        self.ctes(u.with_ctes(), &block, RefRole::Read);
        let target = self.dml_target(u.table());
        let qual = target.as_ref().map(|t| &t.0);
        self.set_clauses(u.setlist(), qual);
        if let Some(from) = u.from_clause() {
            self.table_source(from, &block, RefRole::Read);
        }
        self.expr(u.where_clause(), Ctx::new(RefRole::Read));
        self.order_terms(u.orderby(), Ctx::new(RefRole::Read));
        self.limit(u.limit_clause(), Ctx::new(RefRole::Read));
        self.returning(u.returning());
    }

    fn delete(&mut self, d: n::DeleteStmt<'a>) {
        let block = self.block.clone();
        self.ctes(d.with_ctes(), &block, RefRole::Read);
        let _ = self.dml_target(d.table());
        self.expr(d.where_clause(), Ctx::new(RefRole::Read));
        self.order_terms(d.orderby(), Ctx::new(RefRole::Read));
        self.limit(d.limit_clause(), Ctx::new(RefRole::Read));
        self.returning(d.returning());
    }

    // ---- DDL -----------------------------------------------------------------------------

    fn define(
        &mut self,
        kind: ObjectKind,
        action: DefinitionAction,
        conditional: bool,
        schema: Option<&Nm>,
        name: &Nm,
        parent: Option<(&Nm, Option<&Nm>)>,
        extent: Extent,
        detail: Option<String>,
    ) {
        self.add_def(Definition {
            kind,
            action,
            conditional,
            schema: schema.map(|s| s.norm.clone()),
            name: name.norm.clone(),
            parent: parent.map(|p| p.0.norm.clone()),
            parent_schema: parent.and_then(|p| p.1).map(|s| s.norm.clone()),
            name_extent: name.ext.clone(),
            extent,
            detail,
        });
    }

    /// `TEMP`/`TEMPORARY` objects live in schema `temp`; record that on the definition just
    /// added unless a schema was written.
    fn mark_temp(&mut self, first_new: usize, temporary: n::TemporaryQualifier) {
        if temporary != n::TemporaryQualifier::None {
            if let Some(def) = self.defs.get_mut(first_new) {
                def.schema.get_or_insert_with(|| "temp".to_owned());
            }
        }
    }

    fn table_scope(&mut self, table: &Nm, schema: Option<&Nm>) {
        let region = self.stmt_extent.clone();
        self.add_scope(ScopeEntry {
            kind: ScopeKind::Relation,
            visible_as: table.norm.clone(),
            relation: Some(table.norm.clone()),
            relation_schema: schema.map(|s| s.norm.clone()),
            columns: None,
            region,
        });
    }

    fn create_table(&mut self, t: n::CreateTableStmt<'a>) {
        let schema = self.name(t.schema());
        let Some(table) = self.name(t.table_name()) else {
            self.gap(Gap::UnmodeledConstruct);
            return;
        };
        let whole = self.stmt_extent.clone();
        let first_new = self.defs.len();
        self.define(
            ObjectKind::Table,
            DefinitionAction::Create,
            t.if_not_exists(),
            schema.as_ref(),
            &table,
            None,
            whole,
            None,
        );
        self.mark_temp(first_new, t.temporary());
        let qual = Qual {
            schema: schema.as_ref().map(|s| s.norm.clone()),
            name: table.norm.clone(),
        };
        if t.as_select().is_none() {
            self.table_scope(&table, schema.as_ref());
        }
        if let Some(select) = t.as_select() {
            // The new table's columns are the SELECT's result columns, which are not listed.
            self.gap(Gap::UnmodeledConstruct);
            self.select(Some(select), RefRole::Read);
        }
        if let Some(columns) = t.columns() {
            for col in columns.iter() {
                self.column_def(&col, &table, schema.as_ref(), DefinitionAction::Create);
            }
        }
        if let Some(groups) = t.table_constraints() {
            for group in groups.iter() {
                for item in group.iter() {
                    if let n::TableConstraintItem::TableConstraint(tc) = item {
                        self.table_constraint(tc, &qual);
                    }
                }
            }
        }
    }

    fn column_def(
        &mut self,
        col: &n::ColumnDef<'a>,
        table: &Nm,
        table_schema: Option<&Nm>,
        action: DefinitionAction,
    ) {
        let Some(name) = self.ident(col.column_name()) else {
            return;
        };
        // Node extents of list items start at the head of the list, so only the end is
        // used; the clause begins at the column name.
        let extent = self
            .extent_of(col.node_id())
            .filter(|e| name.ext.end <= e.end)
            .map_or_else(|| name.ext.clone(), |e| ext(name.ext.start, e.end));
        let detail = self.detail(col.type_name());
        self.define(
            ObjectKind::Column,
            action,
            false,
            None,
            &name,
            Some((table, table_schema)),
            extent,
            detail,
        );
        let Some(list) = col.constraints() else { return };
        for item in list.iter() {
            let n::ColumnConstraintItem::ColumnConstraint(cc) = item else {
                continue;
            };
            use n::ColumnConstraintType as K;
            match cc.kind() {
                K::Default => self.expr(
                    cc.default_expr(),
                    Ctx {
                        no_columns: true,
                        ..Ctx::new(RefRole::Schema)
                    },
                ),
                K::Check => self.expr(cc.check_expr(), Ctx::new(RefRole::Schema)),
                K::Generated => self.expr(cc.generated_expr(), Ctx::new(RefRole::Schema)),
                K::References => {
                    if let Some(fk) = cc.fk_clause() {
                        self.fk_clause(&fk);
                    }
                }
                K::NotNull | K::PrimaryKey | K::Unique | K::Collate | K::Null | K::Deferrable => {}
            }
        }
    }

    fn fk_clause(&mut self, fk: &n::ForeignKeyClause<'a>) {
        let Some(parent) = self.name(fk.ref_table()) else {
            return;
        };
        self.push_ref(RefKind::Relation, RefRole::Schema, None, None, &parent, None);
        let qual = Qual {
            schema: None,
            name: parent.norm.clone(),
        };
        self.bound_columns(fk.ref_columns(), RefRole::Schema, Some(&qual));
    }

    fn table_constraint(&mut self, tc: n::TableConstraint<'a>, qual: &Qual) {
        use n::TableConstraintType as K;
        match tc.kind() {
            K::PrimaryKey | K::Unique => {
                if let Some(terms) = tc.pk_columns() {
                    for term in terms.iter() {
                        self.keyed_term(term.expr(), qual);
                    }
                }
            }
            K::Check => self.expr(tc.check_expr(), Ctx::new(RefRole::Schema)),
            K::ForeignKey => {
                self.bound_columns(tc.fk_columns(), RefRole::Schema, Some(qual));
                if let Some(fk) = tc.fk_clause() {
                    self.fk_clause(&fk);
                }
            }
        }
    }

    /// An indexed-column term: a bare column (optionally with COLLATE) is bound to `qual`,
    /// anything else is walked as an expression.
    fn keyed_term(&mut self, e: Option<n::Expr<'a>>, qual: &Qual) {
        let mut e = e;
        while let Some(n::Expr::CollateExpr(c)) = e {
            e = c.expr();
        }
        match e {
            Some(n::Expr::ColumnRef(c)) => self.column_ref(c, Ctx::new(RefRole::Schema), Some(qual)),
            other => self.expr(other, Ctx::new(RefRole::Schema)),
        }
    }

    fn create_virtual(&mut self, v: n::CreateVirtualTableStmt<'a>) {
        // Module arguments define the columns and are module specific.
        self.gap(Gap::UnmodeledConstruct);
        let schema = self.name(v.schema());
        let Some(table) = self.name(v.table_name()) else {
            return;
        };
        let whole = self.stmt_extent.clone();
        self.define(
            ObjectKind::Table,
            DefinitionAction::Create,
            v.if_not_exists(),
            schema.as_ref(),
            &table,
            None,
            whole,
            None,
        );
    }

    fn create_view(&mut self, v: n::CreateViewStmt<'a>) {
        let schema = self.name(v.schema());
        let Some(view) = self.name(v.view_name()) else {
            self.gap(Gap::UnmodeledConstruct);
            return;
        };
        let whole = self.stmt_extent.clone();
        let mut detail = None;
        let mut columns = Vec::new();
        if let Some(list) = v.column_names() {
            for item in list.iter() {
                if let n::Expr::ColumnRef(c) = item {
                    if let Some(nm) = self.name(c.column()) {
                        columns.push(nm);
                    }
                } else {
                    self.gap(Gap::UnmodeledConstruct);
                }
            }
            if let (Some(first), Some(last)) = (columns.first(), columns.last()) {
                let text = self.src[first.ext.start..last.ext.end].to_owned();
                detail = self.detail(&text);
            }
        }
        let first_new = self.defs.len();
        self.define(
            ObjectKind::View,
            DefinitionAction::Create,
            v.if_not_exists(),
            schema.as_ref(),
            &view,
            None,
            whole,
            detail,
        );
        self.mark_temp(first_new, v.temporary());
        for column in &columns {
            self.define(
                ObjectKind::Column,
                DefinitionAction::Create,
                false,
                None,
                column,
                Some((&view, schema.as_ref())),
                column.ext.clone(),
                None,
            );
        }
        self.select(v.select(), RefRole::Schema);
    }

    fn create_index(&mut self, i: n::CreateIndexStmt<'a>) {
        let schema = self.name(i.schema());
        let Some(index) = self.name(i.index_name()) else {
            self.gap(Gap::UnmodeledConstruct);
            return;
        };
        let table = self.name(i.table_name());
        let whole = self.stmt_extent.clone();
        let mut detail = None;
        if let Some(terms) = i.columns() {
            let mut range: Option<(usize, usize)> = None;
            for term in terms.iter() {
                if let Some(e) = self.extent_of(term.node_id()) {
                    range = Some(range.map_or((e.start, e.end), |(s, _)| (s, e.end)));
                }
            }
            if let Some((s, e)) = range {
                let text = self.src[s..e].to_owned();
                detail = self.detail(&text);
            }
        }
        self.define(
            ObjectKind::Index,
            DefinitionAction::Create,
            i.if_not_exists(),
            schema.as_ref(),
            &index,
            table.as_ref().map(|t| (t, None)),
            whole,
            detail,
        );
        let Some(table) = table else {
            self.gap(Gap::UnmodeledConstruct);
            return;
        };
        self.push_ref(RefKind::Relation, RefRole::Schema, None, None, &table, None);
        self.table_scope(&table, None);
        let qual = Qual {
            schema: None,
            name: table.norm.clone(),
        };
        if let Some(terms) = i.columns() {
            for term in terms.iter() {
                self.keyed_term(term.expr(), &qual);
            }
        }
        self.expr(i.where_clause(), Ctx::new(RefRole::Schema));
    }

    fn create_trigger(&mut self, t: n::CreateTriggerStmt<'a>) {
        let schema = self.name(t.schema());
        let Some(trigger) = self.name(t.trigger_name()) else {
            self.gap(Gap::UnmodeledConstruct);
            return;
        };
        let (table_schema, table) = match t.table() {
            Some(q) => (self.ident(q.schema()), self.ident(q.object_name())),
            None => (None, None),
        };
        let whole = self.stmt_extent.clone();
        let first_new = self.defs.len();
        self.define(
            ObjectKind::Trigger,
            DefinitionAction::Create,
            t.if_not_exists(),
            schema.as_ref(),
            &trigger,
            table.as_ref().map(|tb| (tb, table_schema.as_ref())),
            whole.clone(),
            None,
        );
        self.mark_temp(first_new, t.temporary());
        let mut new_row = true;
        let mut old_row = true;
        if let Some(event) = t.event() {
            match event.event_type() {
                n::TriggerEventType::Insert => old_row = false,
                n::TriggerEventType::Delete => new_row = false,
                n::TriggerEventType::Update => {}
            }
            if let Some(table) = &table {
                let qual = Qual {
                    schema: table_schema.as_ref().map(|s| s.norm.clone()),
                    name: table.norm.clone(),
                };
                self.bound_columns(event.columns(), RefRole::Schema, Some(&qual));
            }
        }
        match &table {
            Some(table) => {
                self.push_ref(
                    RefKind::Relation,
                    RefRole::Schema,
                    table_schema.as_ref(),
                    None,
                    table,
                    None,
                );
            }
            None => self.gap(Gap::UnmodeledConstruct),
        }
        for (present, name) in [(new_row, "new"), (old_row, "old")] {
            if present {
                self.add_scope(ScopeEntry {
                    kind: ScopeKind::TriggerRow,
                    visible_as: name.to_owned(),
                    relation: None,
                    relation_schema: None,
                    columns: None,
                    region: whole.clone(),
                });
            }
        }
        self.expr(t.when_expr(), Ctx::new(RefRole::Read));
        if let Some(body) = t.body() {
            for s in body.iter() {
                self.statement(s);
            }
        }
    }

    fn drop_stmt(&mut self, d: n::DropStmt<'a>) {
        let Some(target) = d.target() else {
            self.gap(Gap::UnmodeledConstruct);
            return;
        };
        let schema = self.ident(target.schema());
        let Some(name) = self.ident(target.object_name()) else {
            return;
        };
        let (kind, ref_kind) = match d.object_type() {
            n::DropObjectType::Table => (ObjectKind::Table, RefKind::Relation),
            n::DropObjectType::View => (ObjectKind::View, RefKind::Relation),
            n::DropObjectType::Index => (ObjectKind::Index, RefKind::Index),
            n::DropObjectType::Trigger => (ObjectKind::Trigger, RefKind::Trigger),
        };
        let whole = self.stmt_extent.clone();
        self.define(
            kind,
            DefinitionAction::Drop,
            d.if_exists(),
            schema.as_ref(),
            &name,
            None,
            whole,
            None,
        );
        self.push_ref(ref_kind, RefRole::Schema, schema.as_ref(), None, &name, None);
    }

    fn alter(&mut self, a: n::AlterTableStmt<'a>) {
        let Some(target) = a.target() else {
            self.gap(Gap::UnmodeledConstruct);
            return;
        };
        let schema = self.ident(target.schema());
        let Some(table) = self.ident(target.object_name()) else {
            return;
        };
        let whole = self.stmt_extent.clone();
        self.push_ref(
            RefKind::Relation,
            RefRole::Schema,
            schema.as_ref(),
            None,
            &table,
            None,
        );
        let parent = Some((&table, schema.as_ref()));
        let qual = Qual {
            schema: schema.as_ref().map(|s| s.norm.clone()),
            name: table.norm.clone(),
        };
        match a.op() {
            n::AlterOp::RenameTable => {
                if let Some(new) = self.ident(a.new_name()) {
                    self.define(
                        ObjectKind::Table,
                        DefinitionAction::Rename {
                            to: new.norm,
                            to_extent: new.ext,
                        },
                        false,
                        schema.as_ref(),
                        &table,
                        None,
                        whole,
                        None,
                    );
                }
            }
            n::AlterOp::RenameColumn => {
                let old = self.ident(a.old_name());
                let new = self.ident(a.new_name());
                if let (Some(old), Some(new)) = (old, new) {
                    self.define(
                        ObjectKind::Column,
                        DefinitionAction::Rename {
                            to: new.norm,
                            to_extent: new.ext,
                        },
                        false,
                        None,
                        &old,
                        parent,
                        whole,
                        None,
                    );
                    self.bound_column_name(&old, &qual);
                }
            }
            n::AlterOp::DropColumn => {
                if let Some(old) = self.ident(a.old_name()) {
                    self.define(
                        ObjectKind::Column,
                        DefinitionAction::DropColumn,
                        false,
                        None,
                        &old,
                        parent,
                        whole,
                        None,
                    );
                    self.bound_column_name(&old, &qual);
                }
            }
            n::AlterOp::AddColumn => match a.column() {
                Some(col) => {
                    self.table_scope(&table, schema.as_ref());
                    self.column_def(&col, &table, schema.as_ref(), DefinitionAction::AddColumn);
                }
                None => self.gap(Gap::UnmodeledConstruct),
            },
        }
    }

    /// A Schema-role column reference to a column the statement names directly.
    fn bound_column_name(&mut self, column: &Nm, qual: &Qual) {
        if !self.room() {
            return;
        }
        self.refs.push(Reference {
            kind: RefKind::Column,
            role: RefRole::Schema,
            schema: qual.schema.clone(),
            qualifier: Some(qual.name.clone()),
            name: column.norm.clone(),
            extent: column.ext.clone(),
            full_extent: column.ext.clone(),
        });
    }
}

fn base_to_usize(stmt: &ParsedStatement<'_>, base: syntaqlite::source::StatementBase) -> usize {
    // A statement-relative offset of zero converted to the document gives the base.
    let zero = syntaqlite::source::StmtOffset::from_raw(0);
    let _ = stmt;
    zero.to_doc(base).as_usize()
}
