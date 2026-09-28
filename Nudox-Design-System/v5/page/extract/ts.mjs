// ts.mjs: TypeScript facts for the v5 specimens, read with the TypeScript compiler
// API from real packages on this machine (zod 4.1.8, yaml 2.9.0, and the packages
// beside them that use them). Every property, flag, doc and heritage link comes from
// the parsed declarations; every throw comes from a walked body, with its file:line.
//
//   node v5/page/extract/ts.mjs > v5/page/data/ts-facts.json
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const ts = require("/nix/store/04ybpzcqvfxfss8ca8gs12112nagjwn2-typescript-5.9.3/lib/node_modules/typescript/lib/typescript.js");
const NM = path.join(process.env.HOME, ".config/opencode/node_modules");
const ZOD = path.join(NM, "zod/src/v4");
const YAML = path.join(NM, "yaml/dist");

const roots = [
  path.join(ZOD, "core/errors.ts"), path.join(ZOD, "core/parse.ts"), path.join(ZOD, "core/core.ts"),
  path.join(ZOD, "classic/schemas.ts"), path.join(ZOD, "classic/parse.ts"), path.join(ZOD, "classic/errors.ts"),
  path.join(YAML, "index.d.ts"), path.join(YAML, "schema/yaml-1.1/omap.d.ts"), path.join(YAML, "schema/yaml-1.1/set.d.ts"),
];
const program = ts.createProgram(roots, { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler, noEmit: true, skipLibCheck: true, strict: true });
const checker = program.getTypeChecker();
const sf = (file) => program.getSourceFile(file);
const rel = (file) => path.relative(NM, file);
const posOf = (node) => { const s = node.getSourceFile(); const { line } = s.getLineAndCharacterOfPosition(node.getStart()); return { file: rel(s.fileName), line: line + 1 }; };
const text = (node) => (node ? node.getText() : "");
const docOf = (node) => {
  const js = ts.getJSDocCommentsAndTags(node).filter(ts.isJSDoc);
  const main = js.map((d) => ts.getTextOfJSDocComment(d.comment) || "").join("\n").trim();
  const tags = ts.getJSDocTags(node).map((t) => ({ tag: t.tagName.text, text: ts.getTextOfJSDocComment(t.comment) || "" }));
  return { doc: main, tags };
};
const MOD = { [ts.SyntaxKind.AbstractKeyword]: "AbstractKeyword", [ts.SyntaxKind.ReadonlyKeyword]: "ReadonlyKeyword", [ts.SyntaxKind.StaticKeyword]: "StaticKeyword", [ts.SyntaxKind.ProtectedKeyword]: "ProtectedKeyword", [ts.SyntaxKind.PrivateKeyword]: "PrivateKeyword", [ts.SyntaxKind.DeclareKeyword]: "DeclareKeyword", [ts.SyntaxKind.ExportKeyword]: "ExportKeyword", [ts.SyntaxKind.AsyncKeyword]: "AsyncKeyword" };
const mods = (node) => new Set((ts.getModifiers(node) || []).map((m) => MOD[m.kind] || String(m.kind)));

function find(file, pred) {
  let hit = null;
  (function walk(n) { if (hit) return; if (pred(n)) { hit = n; return; } ts.forEachChild(n, walk); })(sf(file));
  return hit;
}
function findAll(file, pred) {
  const out = [];
  (function walk(n) { if (pred(n)) out.push(n); ts.forEachChild(n, walk); })(sf(file));
  return out;
}
const named = (kind, name) => (n) => n.kind === kind && n.name && n.name.text === name;

// ---------------------------------------------------------------- interfaces, with their own and inherited members
function prop(m) {
  const md = mods(m);
  const d = docOf(m);
  return { name: m.name.getText(), type: text(m.type), optional: !!m.questionToken, readonly: md.has("ReadonlyKeyword"), doc: d.doc, tags: d.tags, ...posOf(m) };
}
function iface(file, name) {
  const n = find(file, named(ts.SyntaxKind.InterfaceDeclaration, name));
  const d = docOf(n);
  const ext = (n.heritageClauses || []).flatMap((h) => h.types.map((t) => text(t.expression)));
  return {
    name, kind: "interface", doc: d.doc, tags: d.tags, ...posOf(n),
    typeParams: (n.typeParameters || []).map((p) => ({ name: p.name.text, constraint: text(p.constraint), dflt: text(p.default) })),
    extends: ext,
    props: n.members.filter(ts.isPropertySignature).map(prop),
    decl: n.getText(),
  };
}

// ---------------------------------------------------------------- the choice: a union of interfaces told apart by `code`
function choice() {
  const file = path.join(ZOD, "core/errors.ts");
  const alias = find(file, named(ts.SyntaxKind.TypeAliasDeclaration, "$ZodIssue"));
  const members = alias.type.types.map((t) => text(t.typeName));
  const base = iface(file, "$ZodIssueBase");
  const cases = members.map((m) => {
    const i = iface(file, m);
    const code = i.props.find((p) => p.name === "code");
    return { ...i, lit: code ? code.type : null, own: i.props.filter((p) => p.name !== "code" && !base.props.some((b) => b.name === p.name)), shadow: i.props.filter((p) => p.name !== "code" && base.props.some((b) => b.name === p.name)).map((p) => p.name) };
  });
  const d = docOf(alias);
  // the classic alias, and its deprecation
  const cfile = path.join(ZOD, "classic/errors.ts");
  const calias = find(cfile, named(ts.SyntaxKind.TypeAliasDeclaration, "ZodIssue"));
  const cd = docOf(calias);
  return { name: "$ZodIssue", decl: alias.getText(), doc: d.doc, ...posOf(alias), base, cases, alias: { name: "ZodIssue", path: "zod · z.ZodIssue", deprecated: (cd.tags.find((t) => t.tag === "deprecated") || {}).text || "", ...posOf(calias) } };
}

// ---------------------------------------------------------------- the record
function record() {
  const file = path.join(ZOD, "core/errors.ts");
  const i = iface(file, "$ZodIssueTooSmall");
  const base = iface(file, "$ZodIssueBase");
  return { ...i, base };
}

// ---------------------------------------------------------------- the callable: schema.parse, and what it throws, walked
function throwsOf(fnNode, bind = {}) {
  const out = [];
  (function walk(n) {
    if (ts.isThrowStatement(n)) {
      const e = n.expression;
      if (ts.isNewExpression(e)) out.push({ cls: text(e.expression).replace(/^core\./, ""), ...posOf(n) });
      else if (ts.isIdentifier(e)) {
        // throw e, where e = new (A ?? B)(…): B is a parameter bound at the call site
        const sym = checker.getSymbolAtLocation(e);
        const decl = sym && sym.valueDeclaration;
        let cls = text(e);
        if (decl && ts.isVariableDeclaration(decl) && decl.initializer && ts.isNewExpression(decl.initializer)) {
          let ctor = decl.initializer.expression;
          while (ts.isParenthesizedExpression(ctor)) ctor = ctor.expression;
          if (ts.isBinaryExpression(ctor) && ctor.operatorToken.kind === ts.SyntaxKind.QuestionQuestionToken) ctor = ctor.right;
          cls = bind[text(ctor)] || text(ctor);
        }
        out.push({ cls, via: text(e), ...posOf(n) });
      }
    }
    ts.forEachChild(n, walk);
  })(fnNode);
  return out;
}
function callable() {
  const sfile = path.join(ZOD, "classic/schemas.ts");
  const zt = find(sfile, named(ts.SyntaxKind.InterfaceDeclaration, "ZodType"));
  const m = zt.members.find((x) => ts.isMethodSignature(x) && x.name.getText() === "parse");
  const safe = zt.members.find((x) => ts.isMethodSignature(x) && x.name.getText() === "safeParse");
  const asyncM = zt.members.find((x) => ts.isMethodSignature(x) && x.name.getText() === "parseAsync");
  const params = m.parameters.map((p) => ({ name: p.name.getText(), type: text(p.type), optional: !!p.questionToken }));
  // step 1: the instance wires parse to parse.parse(inst, …)
  const wire = find(sfile, (n) => ts.isBinaryExpression(n) && n.operatorToken.kind === ts.SyntaxKind.EqualsToken && text(n.left) === "inst.parse");
  // step 2: classic parse = core._parse(ZodRealError)
  const pfile = path.join(ZOD, "classic/parse.ts");
  const pdecl = find(pfile, (n) => ts.isVariableDeclaration(n) && n.name.getText() === "parse");
  let call = pdecl.initializer; while (ts.isAsExpression(call) || ts.isParenthesizedExpression(call)) call = call.expression;
  const errArg = text(call.arguments[0]);
  // step 3: core _parse = (_Err) => (schema, value, …) => { … throw … }
  const cfile = path.join(ZOD, "core/parse.ts");
  const factory = find(cfile, (n) => ts.isVariableDeclaration(n) && n.name.getText() === "_parse");
  const outer = factory.initializer; const param = outer.parameters[0].name.getText();
  const inner = outer.body;
  const throws = throwsOf(inner, { [param]: errArg });
  // the async error's own message
  const core = path.join(ZOD, "core/core.ts");
  const asyncCls = find(core, named(ts.SyntaxKind.ClassDeclaration, "$ZodAsyncError"));
  const superCall = findAll(core, (n) => ts.isCallExpression(n) && n.expression.kind === ts.SyntaxKind.SuperKeyword && n.pos > asyncCls.pos && n.end < asyncCls.end)[0];
  const asyncMsg = superCall ? text(superCall.arguments[0]).replace(/^`|`$/g, "") : "";
  // the ParseContext options (the optional second argument)
  const pc = iface(path.join(ZOD, "core/schemas.ts"), "ParseContext");
  // the error class's issues field
  const zerr = iface(path.join(ZOD, "classic/errors.ts"), "ZodError");
  const ret = text(m.type);
  return {
    name: "parse", owner: "ZodType", decl: m.getText(), ...posOf(m), params, ret,
    doc: docOf(m).doc,
    chain: [{ step: text(wire), ...posOf(wire) }, { step: pdecl.getText().replace(/\s+/g, " ").slice(0, 120), ...posOf(pdecl) }, { step: `_parse(${param}) → (schema, value, _ctx, _params) => …`, ...posOf(factory) }],
    throws: throws.map((t) => ({ ...t, message: t.cls.includes("AsyncError") ? asyncMsg : "" })),
    siblings: { safeParse: { decl: safe.getText(), ...posOf(safe) }, parseAsync: { decl: asyncM.getText(), ...posOf(asyncM) } },
    context: pc, zodError: zerr,
  };
}

// ---------------------------------------------------------------- the contract: an abstract class, and every class that extends it
function member(m) {
  const md = mods(m);
  const d = docOf(m);
  const kind = ts.isMethodDeclaration(m) ? "method" : ts.isPropertyDeclaration(m) ? "field" : ts.isConstructorDeclaration(m) ? "ctor" : "other";
  const name = m.name ? m.name.getText() : "constructor";
  return {
    name, kind, abstract: md.has("AbstractKeyword"), optional: !!m.questionToken, readonly: md.has("ReadonlyKeyword"),
    params: (m.parameters || []).map((p) => ({ name: p.name.getText(), type: text(p.type), optional: !!p.questionToken })),
    type: text(m.type), doc: d.doc, tags: d.tags, ...posOf(m),
  };
}
function allClasses() {
  const out = [];
  for (const s of program.getSourceFiles()) {
    if (!s.fileName.includes("/yaml/dist/")) continue;
    (function walk(n) { if (ts.isClassDeclaration(n) && n.name) out.push(n); ts.forEachChild(n, walk); })(s);
  }
  return out;
}
function contract() {
  const file = path.join(YAML, "nodes/Collection.d.ts");
  const c = find(file, named(ts.SyntaxKind.ClassDeclaration, "Collection"));
  const d = docOf(c);
  const members = c.members.map(member).filter((m) => m.kind !== "other" && m.name !== "constructor" && !m.name.startsWith("["));
  const ext = (c.heritageClauses || []).flatMap((h) => h.types.map((t) => text(t.expression)));
  // the classes that extend it, directly or through another
  const classes = allClasses();
  const parent = new Map(classes.map((k) => [k.name.text, (k.heritageClauses || []).flatMap((h) => h.token === ts.SyntaxKind.ExtendsKeyword ? h.types.map((t) => text(t.expression)) : [])[0] || null]));
  const sat = [];
  for (const k of classes) {
    const chain = []; let p = parent.get(k.name.text);
    while (p && p !== "Collection" && chain.length < 6) { chain.push(p); p = parent.get(p); }
    if (p === "Collection") {
      const own = k.members.map(member).filter((m) => m.kind === "method");
      const fills = members.filter((m) => m.abstract).map((m) => m.name).filter((n) => own.some((o) => o.name === n));
      sat.push({ name: k.name.text, through: chain, fills, abstract: mods(k).has("AbstractKeyword"), ...posOf(k) });
    }
  }
  // takers: public functions whose parameters mention it; uses: every mention by name
  let uses = 0; const usedIn = new Map();
  for (const s of program.getSourceFiles()) {
    if (!s.fileName.includes("/yaml/dist/") || s.fileName === file) continue;
    const n = (s.text.match(/\bCollection\b/g) || []).length;
    if (n) { uses += n; usedIn.set(rel(s.fileName), n); }
  }
  return { name: "Collection", kind: "abstract class", doc: d.doc, ...posOf(c), extends: ext, decl: c.getText().split("\n").slice(0, 1)[0], members, satisfiers: sat, uses, usedIn: [...usedIn].map(([f, n]) => ({ file: f, n })) };
}

// ---------------------------------------------------------------- who else in node_modules mentions these (matched by name: dashed)
function mentions(re, skip) {
  const hits = [];
  const walk = (dir, depth) => {
    if (depth > 6) return;
    let ents; try { ents = fs.readdirSync(dir, { withFileTypes: true }); } catch { return; }
    for (const e of ents) {
      const p = path.join(dir, e.name);
      if (e.isDirectory()) { if (e.name === "tests" || e.name === "locales" || e.name === "src" && skip && p.includes(skip)) continue; walk(p, depth + 1); }
      else if (/\.d\.ts$/.test(e.name) && !(skip && p.includes(skip))) {
        const t = fs.readFileSync(p, "utf8"); const lines = t.split("\n");
        lines.forEach((l, k) => { if (re.test(l)) hits.push({ pkg: rel(p).split("/").slice(0, rel(p).startsWith("@") ? 2 : 1).join("/"), file: rel(p), line: k + 1, text: l.trim().slice(0, 160) }); });
      }
    }
  };
  walk(NM, 0);
  return hits;
}

const zodPkg = JSON.parse(fs.readFileSync(path.join(NM, "zod/package.json"), "utf8"));
const yamlPkg = JSON.parse(fs.readFileSync(path.join(NM, "yaml/package.json"), "utf8"));
const out = {
  packages: { zod: { version: zodPkg.version, license: zodPkg.license, description: zodPkg.description }, yaml: { version: yamlPkg.version, license: yamlPkg.license, description: yamlPkg.description } },
  choice: choice(), record: record(), callable: callable(), contract: contract(),
  mentions: {
    issue: mentions(/\$ZodIssue\b|\bZodIssue\b/, "/zod/"),
    zodImports: mentions(/from ["']zod["']/, "/zod/"),
    collection: mentions(/\bCollection\b/, "/yaml/"),
  },
};
process.stdout.write(JSON.stringify(out, null, 1));
