//! Find by name or by what a call takes and gives, and honest roads through
//! a few calls (gui-plan §8.2). Built once per world, queried only when the
//! field changes; the renderer consumes cached node sets and road stops.
//!
//! Producers, plain type keys, side-input derivation and code spelling come
//! from the shared semantic recipe engine. Only the value-consuming spine
//! is a graph concern. A bounded second shortest-path pass follows it.

use super::model::{Kind, NodeId, World};
use crate::semantics::recipes::{self, How, Kid, Producer, Recipes, Run, Tree};
use crate::semantics::types::{TypeExpr, parse, split_top};
mod capability;
mod proof;
use capability::{Capability, Requirement};
use std::cell::RefCell;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

/// Calls displayed in the find panel.
pub const ROWS: usize = 7;
/// Answers illuminated in the map.
pub const LIT: usize = 400;
/// Roads offered after too few direct calls.
pub const CHAINS: usize = 3;
const CACHE: usize = 8;
const PUBLIC: u32 = u32::MAX;

/// One direct answer and the words the find panel puts below it.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    /// The callable or named symbol.
    pub node: NodeId,
    /// Owner-qualified name.
    pub label: String,
    /// What it takes and gives (empty for name search).
    pub shape: String,
}

/// A step on the value-consuming spine; other inputs ride alongside it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainStep {
    /// Callable, variant, literal or default symbol.
    pub node: NodeId,
    /// The name used in the rail.
    pub verb: String,
    /// The spine input key.
    pub input: String,
    /// The output key.
    pub output: String,
    /// Other inputs in plain words, with meaningful argument names.
    pub riders: Vec<String>,
    /// May fail.
    pub fails: bool,
    /// May give nothing.
    pub maybe: bool,
}

/// Calls on the same type fold into one place on the graph road.
#[derive(Clone, Debug, PartialEq)]
pub struct RoadStop {
    /// The place: a type, or a free function.
    pub node: NodeId,
    /// `Language · from › name`.
    pub label: String,
    /// Calls folded into the place, in road order.
    pub calls: Vec<NodeId>,
    /// The value supplied by the person searching.
    pub yours: bool,
}

/// A bounded derivation that actually consumes a supplied value.
#[derive(Clone, Debug, PartialEq)]
pub struct Chain {
    /// The ranking cost, including side inputs and distance from home.
    pub cost: f64,
    /// The actual supplied type key (even when a trait carries it).
    pub from: String,
    /// The output key.
    pub output: String,
    /// A trait the supplied value is passed as, if any.
    pub via: Option<String>,
    /// Calls, in order (at most three).
    pub steps: Vec<ChainStep>,
    /// Supplied type, calls, and output type, when named.
    pub path: Vec<NodeId>,
    /// The path folded into places for painting and framing.
    pub stops: Vec<RoadStop>,
    /// The panel's one-line summary.
    pub brief: String,
    /// The plate's rail in words.
    pub rail: String,
    /// Shared recipe grammar, with intermediate optional steps unwrapped.
    pub code: String,
}

/// An immutable query result, reused by the find panel and renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct Search {
    /// Whether the query asks what a callable takes and gives.
    pub shaped: bool,
    /// At most seven direct answers.
    pub rows: Vec<Match>,
    /// At most 400 direct answers, ranked in the same order as the panel.
    pub lit: Vec<NodeId>,
    /// Constant-time membership for painting.
    pub lit_set: HashSet<NodeId>,
    /// Distinct packages containing illuminated answers.
    pub packages: usize,
    /// Up to three roads, offered when fewer than three calls answer.
    pub chains: Vec<Chain>,
}

#[derive(Clone, Default)]
struct Words {
    keys: HashSet<String>,
    maybe: bool,
}
struct Shape {
    ins: Vec<Words>,
    out: Option<Words>,
}

/// Indexes and bounded query caches for one immutable world.
pub struct Discovery {
    capabilities: Arc<HashMap<String, HashSet<Capability>>>,
    requirements: Arc<HashMap<(usize, usize), Requirement>>,
    proof: Arc<Vec<proof::TypeProof>>,
    semantic_names: Arc<crate::semantics::names::Names>,
    package_items: Arc<Vec<Vec<NodeId>>>,
    recipes: Recipes,
    names: Arc<Vec<(NodeId, String, String)>>,
    types: Arc<HashMap<String, Vec<NodeId>>>,
    traits: Arc<HashMap<String, Vec<String>>>,
    by_in: Arc<HashMap<String, Vec<usize>>>,
    by_out: Arc<HashMap<String, Vec<usize>>>,
    cache: RefCell<VecDeque<(String, Rc<Search>)>>,
    seeded: RefCell<VecDeque<(Vec<String>, Rc<Run>)>>,
}

/// Transferable indexes built by the graph's background loading task.
/// Attaching query caches with [`Discovery::from_prepared`] does no parsing.
#[derive(Clone)]
pub struct PreparedDiscovery {
    capabilities: Arc<HashMap<String, HashSet<Capability>>>,
    requirements: Arc<HashMap<(usize, usize), Requirement>>,
    proof: Arc<Vec<proof::TypeProof>>,
    semantic_names: Arc<crate::semantics::names::Names>,
    package_items: Arc<Vec<Vec<NodeId>>>,
    recipes: recipes::PreparedRecipes,
    names: Arc<Vec<(NodeId, String, String)>>,
    types: Arc<HashMap<String, Vec<NodeId>>>,
    traits: Arc<HashMap<String, Vec<String>>>,
    by_in: Arc<HashMap<String, Vec<usize>>>,
    by_out: Arc<HashMap<String, Vec<usize>>>,
}

impl Discovery {
    /// Builds an index synchronously (small fixtures and existing scenes).
    #[must_use]
    pub fn new(world: &World) -> Self {
        Self::from_prepared(Self::prepare(world))
    }

    /// Parses and indexes on a background worker; the result is `Send`.
    #[must_use]
    pub fn prepare(world: &World) -> PreparedDiscovery {
        let recipes = Recipes::new(world);
        let semantic_names = Arc::new(crate::semantics::names::Names::new(world));
        let mut package_items = vec![Vec::new(); world.packages.len()];
        let mut types: HashMap<String, Vec<NodeId>> = HashMap::new();
        let mut names = Vec::with_capacity(world.len());
        let mut traits = HashMap::new();
        for (a, node) in world.nodes.iter().enumerate() {
            let Ok(i) = NodeId::try_from(a) else { continue };
            if node.parent.is_none() {
                package_items[node.pkg as usize].push(i);
            }
            if !node.orphan {
                names.push((i, node.name.to_lowercase(), world.qual(i).to_lowercase()));
            }
            if node.parent.is_none() && node.kind.is_type_like() {
                types.entry(node.name.to_lowercase()).or_default().push(i);
                let mut ts: Vec<String> = node.derives.iter().map(|s| s.to_string()).collect();
                ts.extend(
                    node.impls
                        .iter()
                        .filter_map(|imp| imp.trait_)
                        .map(|t| world.node(t).name.to_string()),
                );
                ts.extend(node.impls_ext.iter().map(|s| {
                    s.split('<')
                        .next()
                        .unwrap_or(s)
                        .rsplit("::")
                        .next()
                        .unwrap_or(s)
                        .trim()
                        .to_owned()
                }));
                ts.sort();
                ts.dedup();
                traits.insert(
                    format!("#{i}"),
                    ts.into_iter().map(|s| format!("any {s}")).collect(),
                );
            }
        }
        let proof = proof::index(world, &recipes, &semantic_names);
        let mut by_in: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_out: HashMap<String, Vec<usize>> = HashMap::new();
        for (q, e) in recipes.table().iter().enumerate() {
            if !proof[q].eligible() {
                continue;
            }
            for k in &e.ins {
                let qs = by_in.entry(k.clone()).or_default();
                if qs.last() != Some(&q) {
                    qs.push(q);
                }
            }
            by_out.entry(e.out.clone()).or_default().push(q);
        }
        let (capabilities, requirements) = capability::index(world, &recipes);
        PreparedDiscovery {
            capabilities: Arc::new(capabilities),
            requirements: Arc::new(requirements),
            proof: Arc::new(proof),
            semantic_names,
            package_items: Arc::new(package_items),
            recipes: recipes.into_prepared(),
            names: Arc::new(names),
            types: Arc::new(types),
            traits: Arc::new(traits),
            by_in: Arc::new(by_in),
            by_out: Arc::new(by_out),
        }
    }

    /// Attaches empty caches after loading; the expensive work is finished.
    #[must_use]
    pub fn from_prepared(data: PreparedDiscovery) -> Self {
        Self {
            capabilities: data.capabilities,
            requirements: data.requirements,
            proof: data.proof,
            semantic_names: data.semantic_names,
            package_items: data.package_items,
            recipes: Recipes::from_prepared(data.recipes),
            names: data.names,
            types: data.types,
            traits: data.traits,
            by_in: data.by_in,
            by_out: data.by_out,
            cache: RefCell::new(VecDeque::new()),
            seeded: RefCell::new(VecDeque::new()),
        }
    }

    /// Shares immutable producers and indexes with a query worker.
    #[must_use]
    pub fn prepared(&self) -> PreparedDiscovery {
        PreparedDiscovery {
            capabilities: self.capabilities.clone(),
            requirements: self.requirements.clone(),
            proof: self.proof.clone(),
            semantic_names: self.semantic_names.clone(),
            package_items: self.package_items.clone(),
            recipes: self.recipes.prepared(),
            names: self.names.clone(),
            types: self.types.clone(),
            traits: self.traits.clone(),
            by_in: self.by_in.clone(),
            by_out: self.by_out.clone(),
        }
    }

    /// Computes a `Send` result without rebuilding the indexes.
    /// The UI should accept it only if its query generation is still current.
    #[must_use]
    pub fn query_prepared(data: PreparedDiscovery, world: &World, query: &str) -> Search {
        let discovery = Self::from_prepared(data);
        let result = discovery.query(world, query);
        drop(discovery);
        Rc::try_unwrap(result).unwrap_or_else(|result| result.as_ref().clone())
    }

    /// Retained query and seeded-run entries (each cache is capped at eight).
    /// This is constant-time instrumentation for long interaction soaks.
    #[must_use]
    pub fn cache_len(&self) -> usize {
        self.cache.borrow().len() + self.seeded.borrow().len()
    }

    /// A previous immutable result; cache hits never start a worker.
    #[must_use]
    pub fn cached(&self, query: &str) -> Option<Rc<Search>> {
        self.cache
            .borrow()
            .iter()
            .find(|(q, _)| q == query.trim())
            .map(|(_, result)| result.clone())
    }

    /// Attaches an accepted worker result to the bounded UI cache.
    #[must_use]
    pub fn remember(&self, query: &str, result: Search) -> Rc<Search> {
        if let Some(cached) = self.cached(query) {
            return cached;
        }
        let result = Rc::new(result);
        let mut cache = self.cache.borrow_mut();
        if cache.len() == CACHE {
            cache.pop_front();
        }
        cache.push_back((query.trim().to_owned(), result.clone()));
        result
    }

    /// The semantic name index, prepared off the UI thread once per world.
    #[must_use]
    pub fn semantic_names(&self) -> &crate::semantics::names::Names {
        &self.semantic_names
    }

    /// Top-level package items, without an action-time scan of the whole world.
    #[must_use]
    pub fn package_items(&self, package: u32) -> &[NodeId] {
        self.package_items
            .get(package as usize)
            .map_or(&[], Vec::as_slice)
    }

    /// The shared producer engine (for inspecting a result or rendering links).
    #[must_use]
    pub fn recipes(&self) -> &Recipes {
        &self.recipes
    }

    /// Returns a cached result; call when input changes, never every frame.
    #[must_use]
    pub fn query(&self, world: &World, query: &str) -> Rc<Search> {
        let query = query.trim().to_owned();
        if let Some(value) = self.cached(&query) {
            return value;
        }
        let shaped = is_shape(&query);
        let shape = shaped.then(|| self.parse_shape(world, &query));
        let lit = shape
            .as_ref()
            .map_or_else(|| self.named(world, &query), |s| self.shaped(world, s));
        let rows = lit
            .iter()
            .take(ROWS)
            .map(|&node| Match {
                node,
                label: full_name(world, node),
                shape: if shaped {
                    self.label(world, node)
                } else {
                    String::new()
                },
            })
            .collect();
        let packages = lit
            .iter()
            .map(|&i| world.node(i).pkg)
            .collect::<HashSet<_>>()
            .len();
        let chains = shape
            .filter(|_| lit.len() < 3)
            .map_or_else(Vec::new, |s| self.chains(world, &s));
        let result = Rc::new(Search {
            shaped,
            rows,
            lit_set: lit.iter().copied().collect(),
            lit,
            packages,
            chains,
        });
        let mut cache = self.cache.borrow_mut();
        if cache.len() == CACHE {
            cache.pop_front();
        }
        cache.push_back((query, result.clone()));
        result
    }

    fn named(&self, world: &World, query: &str) -> Vec<NodeId> {
        if query.is_empty() {
            return Vec::new();
        }
        let query = query.to_lowercase();
        // A package prefix may be separated by whitespace as well as ::.
        let (place, last) = query
            .rsplit_once("::")
            .or_else(|| query.rsplit_once(' '))
            .unwrap_or(("", &query));
        let mut scored = Vec::new();
        for (i, name, qual) in self.names.iter() {
            let Some(at) = name.find(last) else { continue };
            if !place.is_empty() && !qual.contains(place) {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let score = (if name == last {
                3.0
            } else if at == 0 {
                1.6
            } else {
                0.6
            }) + f64::from(world.importance(*i)) * 1.5
                + if world.node(*i).parent.is_none() {
                    0.4
                } else {
                    0.0
                }
                + if world.yours(*i) { 0.2 } else { 0.0 }
                - name.len() as f64 * 0.01;
            scored.push((score, *i));
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(LIT).map(|(_, i)| i).collect()
    }

    fn parse_shape(&self, world: &World, query: &str) -> Shape {
        let mut q = query.replace('→', "->").replace("=>", "->");
        let lower = q.to_lowercase();
        if lower.starts_with("takes ") {
            q = q[6..].to_owned();
            if let Some(at) = q.to_lowercase().find(" gives ") {
                q.replace_range(at..at + 7, " -> ");
            }
        } else if lower.starts_with("gives ") {
            q = format!("-> {}", &q[6..]);
        }
        let (left, right) = q
            .split_once("->")
            .map_or((q.as_str(), None), |(l, r)| (l, Some(r)));
        let ins = split_top(&left.replace(" and ", ","), ',')
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .map(|s| self.words(world, &s))
            .collect();
        let out = right
            .filter(|r| !r.trim().is_empty())
            .map(|r| self.words(world, r));
        Shape { ins, out }
    }

    fn words(&self, world: &World, source: &str) -> Words {
        let mut s = source.trim();
        let mut maybe = false;
        if let Some(rest) = s.strip_suffix('?') {
            s = rest.trim();
            maybe = true;
        }
        let lower = s.to_lowercase();
        for article in ["a ", "an ", "the ", "some "] {
            if lower.starts_with(article) {
                let mut out = self.words(world, &s[article.len()..]);
                out.maybe |= maybe;
                return out;
            }
        }
        for prefix in [
            "maybe ",
            "option of ",
            "optional of ",
            "optional ",
            "option ",
        ] {
            if lower.starts_with(prefix) {
                let mut out = self.words(world, &s[prefix.len()..]);
                out.maybe = true;
                return out;
            }
        }
        for prefix in ["list of ", "vec of ", "many ", "all ", "several "] {
            if lower.starts_with(prefix) {
                let out = self.words(world, &s[prefix.len()..]);
                let byte = matches!(
                    s[prefix.len()..].trim().to_lowercase().as_str(),
                    "u8" | "byte"
                );
                return Words {
                    keys: out
                        .keys
                        .into_iter()
                        .map(|k| {
                            if byte {
                                "bytes".to_owned()
                            } else {
                                format!("list of {k}")
                            }
                        })
                        .collect(),
                    maybe,
                };
            }
        }
        if let Some(bound) = s.strip_prefix("any ") {
            let expr = parse(bound);
            if let Some(last) = expr.last() {
                return Words {
                    keys: HashSet::from([format!("any {last}")]),
                    maybe,
                };
            }
        }
        if lower.starts_with("map ")
            && let Some((key, value)) = s[4..].split_once(" to ")
        {
            let keys = self.words(world, key).keys;
            let values = self.words(world, value).keys;
            return Words {
                keys: keys
                    .into_iter()
                    .flat_map(|k| values.iter().map(move |v| format!("map {k} to {v}")))
                    .take(LIT)
                    .collect(),
                maybe,
            };
        }
        let plain = match lower.as_str() {
            "text" | "string" | "str" | "name" | "&str" | "osstr" | "osstring" => Some("text"),
            "path" | "file" | "pathbuf" | "&path" | "dir" | "directory" => Some("path"),
            "number" | "int" | "integer" | "count" | "size" | "float" | "index" | "byte"
            | "duration" => Some("number"),
            "bool" | "boolean" | "flag" | "yes or no" => Some("bool"),
            "bytes" | "[u8]" | "&[u8]" | "vec<u8>" | "buffer" => Some("bytes"),
            "nothing" | "()" | "unit" | "" => Some("nothing"),
            "char" | "character" => Some("char"),
            "anything" | "any" | "whatever" => Some("any"),
            _ => None,
        };
        if let Some(key) = plain {
            return Words {
                keys: HashSet::from([key.to_owned()]),
                maybe,
            };
        }
        let mut out = self.expr_words(world, &parse(s));
        out.maybe |= maybe;
        out
    }

    fn expr_words(&self, world: &World, expr: &TypeExpr) -> Words {
        let one = |k: &str| Words {
            keys: HashSet::from([k.to_owned()]),
            maybe: false,
        };
        match expr {
            TypeExpr::Ref { inner, .. } => self.expr_words(world, inner),
            TypeExpr::Ptr { .. } => Words::default(),
            TypeExpr::Slice(inner) | TypeExpr::Array { inner, .. } => {
                let byte = inner.last() == Some("u8");
                let w = self.expr_words(world, inner);
                Words {
                    keys: w
                        .keys
                        .into_iter()
                        .map(|k| {
                            if byte {
                                "bytes".to_owned()
                            } else {
                                format!("list of {k}")
                            }
                        })
                        .collect(),
                    maybe: false,
                }
            }
            TypeExpr::Named { path, args } => {
                let last = path.last().map_or("", String::as_str);
                if recipes::represented_arguments(last).is_some_and(|count| args.len() != count) {
                    return Words::default();
                }
                match last {
                    "Option" | "Result" | "Box" | "Rc" | "Arc" | "Cow" | "Pin" | "RefCell"
                    | "Cell" | "Mutex" | "RwLock" | "Ref" | "RefMut" | "AsRef" | "Into"
                    | "Borrow" | "Weak" | "ManuallyDrop" => {
                        let mut out = args
                            .first()
                            .map_or_else(Words::default, |a| self.expr_words(world, a));
                        out.maybe |= last == "Option";
                        return out;
                    }
                    "Vec" | "VecDeque" | "SmallVec" | "LinkedList" | "HashSet" | "BTreeSet"
                    | "IndexSet" | "BinaryHeap" | "Iterator" | "IntoIterator" | "Peekable" => {
                        let Some(a) = args.first() else {
                            return Words::default();
                        };
                        let w = self.expr_words(world, a);
                        return Words {
                            keys: w
                                .keys
                                .into_iter()
                                .map(|k| {
                                    if a.last() == Some("u8") {
                                        "bytes".to_owned()
                                    } else {
                                        format!("list of {k}")
                                    }
                                })
                                .collect(),
                            maybe: false,
                        };
                    }
                    "HashMap" | "BTreeMap" | "IndexMap" => {
                        let Some((key, value)) = args.first().zip(args.get(1)) else {
                            return Words::default();
                        };
                        let keys = self.expr_words(world, key).keys;
                        let values = self.expr_words(world, value).keys;
                        return Words {
                            keys: keys
                                .into_iter()
                                .flat_map(|k| values.iter().map(move |v| format!("map {k} to {v}")))
                                .take(LIT)
                                .collect(),
                            maybe: false,
                        };
                    }
                    "String" | "str" | "OsString" | "OsStr" => return one("text"),
                    "Path" | "PathBuf" => return one("path"),
                    "bool" => return one("bool"),
                    "char" => return one("char"),
                    "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32"
                    | "i64" | "i128" | "isize" | "f32" | "f64" | "NonZeroU32" | "NonZeroU64"
                    | "NonZeroUsize" | "Duration" => return one("number"),
                    _ => {}
                }
                // A nominal key cannot distinguish `Wrapper<A>` from
                // `Wrapper<B>`; an applied query therefore has no proved key.
                if !args.is_empty() {
                    return Words::default();
                }
                let qualifier = path
                    .get(..path.len().saturating_sub(1))
                    .unwrap_or_default()
                    .join("::")
                    .to_lowercase();
                let keys = self
                    .types
                    .get(&last.to_lowercase())
                    .into_iter()
                    .flatten()
                    .filter(|&&i| {
                        if qualifier.is_empty() {
                            return true;
                        }
                        let node = world.node(i);
                        let package = world.packages[node.pkg as usize]
                            .name
                            .to_lowercase()
                            .replace('-', "_");
                        let module = world.modules[node.module as usize].path.to_lowercase();
                        let full = if module.is_empty() {
                            package
                        } else {
                            format!("{package}::{module}")
                        };
                        let actual = world.qual(i).to_lowercase().replace('-', "_");
                        let written = qualifier.replace('-', "_");
                        actual == written
                            || full == written
                            || actual.ends_with(&format!("::{written}"))
                            || full.ends_with(&format!("::{written}"))
                    })
                    .map(|i| format!("#{i}"))
                    .collect();
                Words { keys, maybe: false }
            }
            TypeExpr::Any(bounds) => Words {
                keys: bounds
                    .iter()
                    .filter_map(TypeExpr::last)
                    .map(|b| format!("any {b}"))
                    .collect(),
                maybe: false,
            },
            TypeExpr::Infer | TypeExpr::Assoc { .. } => one("any"),
            TypeExpr::Never => one("never"),
            TypeExpr::Tuple(parts) if parts.is_empty() => one("nothing"),
            TypeExpr::Tuple(parts) => {
                let keys: Vec<_> = parts
                    .iter()
                    .map(|a| self.expr_words(world, a).keys)
                    .collect();
                let mut combos = vec![Vec::<String>::new()];
                for ks in keys {
                    combos = combos
                        .into_iter()
                        .flat_map(|a| {
                            ks.iter().map(move |k| {
                                let mut b = a.clone();
                                b.push(k.clone());
                                b
                            })
                        })
                        .take(LIT)
                        .collect();
                }
                Words {
                    keys: combos
                        .into_iter()
                        .map(|a| format!("({})", a.join(", ")))
                        .collect(),
                    maybe: false,
                }
            }
            TypeExpr::Binding { ty, .. } => self.expr_words(world, ty),
            TypeExpr::Func { .. } => one("a function"),
        }
    }

    fn via(&self, words: &Words) -> HashSet<String> {
        words
            .keys
            .iter()
            .filter_map(|k| self.traits.get(k))
            .flatten()
            .cloned()
            .collect()
    }

    fn shaped(&self, world: &World, shape: &Shape) -> Vec<NodeId> {
        if shape.out.as_ref().is_some_and(|o| o.keys.is_empty())
            || shape.ins.iter().any(|i| i.keys.is_empty())
            || (shape.ins.is_empty() && shape.out.is_none())
        {
            return Vec::new();
        }
        let via: Vec<_> = shape.ins.iter().map(|i| self.via(i)).collect();
        // Start with the narrowest index rather than scanning every producer.
        let candidates: Vec<usize> = if let Some(out) = &shape.out {
            let mut qs = Vec::new();
            for k in &out.keys {
                for key in [k.clone(), format!("list of {k}")] {
                    qs.extend(self.by_out.get(&key).into_iter().flatten().copied());
                }
            }
            qs.sort_unstable();
            qs.dedup();
            qs
        } else {
            let mut qs = Vec::new();
            for (a, input) in shape.ins.iter().enumerate() {
                for k in input.keys.iter().chain(&via[a]) {
                    qs.extend(self.by_in.get(k).into_iter().flatten().copied());
                }
            }
            if shape.ins.iter().any(|i| i.keys.contains("any")) {
                qs.extend(
                    self.recipes
                        .table()
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.ins.iter().any(|k| k.starts_with("any ")))
                        .map(|(q, _)| q),
                );
            }
            qs.sort_unstable();
            qs.dedup();
            qs
        };
        let mut scored = Vec::new();
        for q in candidates {
            let e = self.recipes.producer(q);
            if !matches!(e.how, How::Call | How::Method) || !self.proof[q].eligible() {
                continue;
            }
            let mut score = 0.0;
            if let Some(out) = &shape.out {
                if out.keys.contains(&e.out) {
                    score += 3.0;
                } else if out.keys.iter().any(|k| e.out == format!("list of {k}")) {
                    score += 1.0;
                } else {
                    continue;
                }
                if out.maybe {
                    score += if e.maybe { 0.5 } else { -0.4 };
                }
            }
            let permitted: Vec<Vec<bool>> = shape
                .ins
                .iter()
                .map(|input| {
                    e.ins
                        .iter()
                        .enumerate()
                        .map(|(j, _)| self.satisfies(q, j, &input.keys))
                        .collect()
                })
                .collect();
            let Some(input_score) = self.unified_score(world, q, shape, &via, &e.ins, &permitted)
            else {
                continue;
            };
            score += input_score;
            score += f64::from(world.importance(e.node)) * 1.5
                + if world.yours(e.node) { 0.2 } else { 0.0 }
                + if world.node(e.node).vis.as_deref() == Some("pub") {
                    0.3
                } else {
                    0.0
                };
            scored.push((score, q));
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut seen = HashSet::new();
        scored
            .into_iter()
            .filter_map(|(_, q)| {
                let e = self.recipes.producer(q);
                let n = world.node(e.node);
                let tag = (
                    n.parent,
                    n.name.clone(),
                    e.ins.clone(),
                    e.out.clone(),
                    e.fails,
                    e.maybe,
                );
                seen.insert(tag).then_some(e.node)
            })
            .take(LIT)
            .collect()
    }

    /// A callable's shape in plain words.
    #[must_use]
    pub fn label(&self, world: &World, node: NodeId) -> String {
        let Some(e) = self
            .recipes
            .table()
            .iter()
            .find(|e| e.node == node && matches!(e.how, How::Call | How::Method))
        else {
            return String::new();
        };
        format!(
            "({}) → {}{}{}",
            e.ins
                .iter()
                .map(|k| recipes::key_words(world, k))
                .collect::<Vec<_>>()
                .join(", "),
            if e.maybe { "maybe " } else { "" },
            recipes::key_words(world, &e.out),
            if e.fails { " or fails" } else { "" }
        )
    }

    fn seeded(&self, world: &World, have: &HashSet<String>) -> Rc<Run> {
        let mut tag: Vec<_> = have.iter().cloned().collect();
        tag.sort();
        if let Some((_, run)) = self.seeded.borrow().iter().find(|(keys, _)| *keys == tag) {
            return run.clone();
        }
        let run = Rc::new(self.recipes.run_with_filter(world, PUBLIC, have, |q, e| {
            self.proof[q].eligible()
                && e.ins
                    .iter()
                    .enumerate()
                    .all(|(j, key)| self.satisfies_key(q, j, key) || self.satisfies(q, j, have))
        }));
        let mut cache = self.seeded.borrow_mut();
        if cache.len() == CACHE {
            cache.pop_front();
        }
        cache.push_back((tag, run.clone()));
        run
    }

    fn chains(&self, world: &World, shape: &Shape) -> Vec<Chain> {
        let Some(out) = &shape.out else {
            return Vec::new();
        };
        if out.keys.is_empty()
            || shape.ins.is_empty()
            || shape.ins.iter().any(|i| i.keys.is_empty())
        {
            return Vec::new();
        }
        let have: HashSet<String> = shape
            .ins
            .iter()
            .flat_map(|i| i.keys.iter().cloned())
            .collect();
        if have.iter().all(|k| recipes::is_ground(k)) {
            return Vec::new();
        }
        let mut traits = HashMap::new();
        let mut ordered: Vec<_> = have.iter().collect();
        ordered.sort();
        for k in ordered {
            for t in self.traits.get(k).into_iter().flatten() {
                traits.entry(t.clone()).or_insert_with(|| k.clone());
            }
        }
        let r0 = self.seeded(world, &have);
        let home: HashSet<_> = have
            .iter()
            .chain(&out.keys)
            .filter_map(|k| node_of(k))
            .map(|i| world.node(i).pkg)
            .collect();
        let away = |e: &Producer| {
            if !home.is_empty() && !home.contains(&world.node(e.node).pkg) {
                0.6
            } else {
                0.0
            }
        };
        let side_key = |q: usize, j: usize, a: usize, source: &str| {
            let same_variable = self
                .requirements
                .get(&(q, j))
                .and_then(|r| r.variable.as_deref())
                .zip(
                    self.requirements
                        .get(&(q, a))
                        .and_then(|r| r.variable.as_deref()),
                )
                .is_some_and(|(left, right)| left == right);
            if same_variable {
                source.to_owned()
            } else {
                self.recipes.producer(q).ins[a].clone()
            }
        };
        let others = |q: usize, j: usize, source: &str| {
            self.recipes
                .producer(q)
                .ins
                .iter()
                .enumerate()
                .filter(|(a, key)| *a != j && *key != "nothing")
                .map(|(a, _)| {
                    r0.cost
                        .get(&side_key(q, j, a, source))
                        .copied()
                        .unwrap_or(f64::INFINITY)
                        + 0.3
                })
                .sum::<f64>()
        };
        let mut cost = HashMap::new();
        let mut best: HashMap<String, (usize, usize)> = HashMap::new();
        let mut done = HashSet::new();
        let mut heap = BinaryHeap::new();
        for k in &have {
            cost.insert(k.clone(), 0.0);
            heap.push(Queued(0.0, k.clone()));
        }
        for k in traits.keys().filter(|k| !have.contains(*k)) {
            cost.insert(k.clone(), 0.1);
            heap.push(Queued(0.1, k.clone()));
        }
        while let Some(Queued(c, k)) = heap.pop() {
            if !done.insert(k.clone()) {
                continue;
            }
            if c > 3.6 {
                break;
            }
            if out.keys.contains(&k) && !have.contains(&k) {
                continue;
            }
            for &q in self.by_in.get(&k).into_iter().flatten() {
                let e = self.recipes.producer(q);
                if matches!(e.out.as_str(), "never" | "?" | "nothing")
                    || have.contains(&e.out)
                    || e.ins.contains(&e.out)
                    || !Recipes::usable(world, e, PUBLIC)
                {
                    continue;
                }
                if recipes::is_ground(&e.out) && !out.keys.contains(&e.out) {
                    continue;
                }
                if self.hollow(world, &k, e, &best) || identity(&world.node(e.node).name) {
                    continue;
                }
                let Some(j) = e.ins.iter().position(|x| x == &k) else {
                    continue;
                };
                let source = traits.get(&k).unwrap_or(&k);
                if !self.satisfies_key(q, j, source) {
                    continue;
                }
                let s = c + self.recipes.weight(world, e) + away(e) + others(q, j, source);
                if s.is_finite()
                    && !done.contains(&e.out)
                    && s < cost.get(&e.out).copied().unwrap_or(f64::INFINITY)
                {
                    cost.insert(e.out.clone(), s);
                    best.insert(e.out.clone(), (q, j));
                    heap.push(Queued(s, e.out.clone()));
                }
            }
        }
        let mut candidates = Vec::new();
        let mut qs: Vec<_> = out
            .keys
            .iter()
            .flat_map(|k| self.by_out.get(k).into_iter().flatten().copied())
            .collect();
        qs.sort_unstable();
        qs.dedup();
        for q in qs {
            let e = self.recipes.producer(q);
            if have.contains(&e.out)
                || e.ins.contains(&e.out)
                || !Recipes::usable(world, e, PUBLIC)
                || identity(&world.node(e.node).name)
            {
                continue;
            }
            let mut pick = None;
            for (j, k) in e.ins.iter().enumerate() {
                if !done.contains(k) || self.hollow(world, k, e, &best) {
                    continue;
                }
                let source = traits.get(k).unwrap_or(k);
                if !self.satisfies_key(q, j, source) {
                    continue;
                }
                let s = cost.get(k).copied().unwrap_or(f64::INFINITY)
                    + self.recipes.weight(world, e)
                    + away(e)
                    + others(q, j, source)
                    + if out.maybe && !e.maybe { 0.2 } else { 0.0 };
                if s.is_finite() && pick.is_none_or(|(c, _)| s < c) {
                    pick = Some((s, j));
                }
            }
            if let Some((c, j)) = pick {
                candidates.push((c, q, j));
            }
        }
        candidates.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| {
                    world
                        .importance(self.recipes.producer(b.1).node)
                        .total_cmp(&world.importance(self.recipes.producer(a.1).node))
                })
                .then(a.1.cmp(&b.1))
        });
        let mut chains: Vec<Chain> = Vec::new();
        let mut groups = HashSet::new();
        for (c, q, j) in candidates {
            if c > 3.6 || chains.first().is_some_and(|first| c > first.cost + 1.5) {
                continue;
            }
            let e = self.recipes.producer(q);
            if have.contains(&e.ins[j]) || traits.contains_key(&e.ins[j]) {
                continue;
            }
            let mut spine = vec![(q, j)];
            let mut k = e.ins[j].clone();
            let mut seen = HashSet::new();
            while !have.contains(&k) && !traits.contains_key(&k) {
                if !seen.insert(k.clone()) || spine.len() >= 3 {
                    break;
                }
                let Some(&(b, a)) = best.get(&k) else { break };
                spine.insert(0, (b, a));
                k = self.recipes.producer(b).ins[a].clone();
            }
            if !have.contains(&k) && !traits.contains_key(&k) {
                continue;
            }
            let via = (!have.contains(&k)).then(|| k.clone());
            let from = traits
                .get(&k)
                .filter(|_| via.is_some())
                .cloned()
                .unwrap_or(k);
            let tag: Vec<_> = spine
                .iter()
                .map(|&(q, _)| self.verb(world, self.recipes.producer(q)))
                .collect();
            if !groups.insert(tag) {
                continue;
            }
            let mut tree = None;
            let mut steps = Vec::new();
            for &(q, j) in &spine {
                let e = self.recipes.producer(q);
                let current_input = steps
                    .last()
                    .map_or_else(|| from.clone(), |step: &ChainStep| step.output.clone());
                let mut kids = Vec::new();
                let mut riders = Vec::new();
                for (a, k) in e.ins.iter().enumerate() {
                    let name = e.names.get(a).cloned().unwrap_or_default();
                    if a == j {
                        let key = if tree.is_none() {
                            from.clone()
                        } else {
                            k.clone()
                        };
                        kids.push(Kid {
                            key,
                            name,
                            node: tree.take(),
                            opaque: false,
                        });
                    } else {
                        let bound_key = side_key(q, j, a, &current_input);
                        let k = &bound_key;
                        if k != "nothing" {
                            riders.push(format!(
                                "{}{}",
                                recipes::key_words(world, k),
                                if recipes::shown_name(&name) {
                                    format!(" {name}")
                                } else {
                                    String::new()
                                }
                            ));
                        }
                        let node = if have.contains(k) || recipes::is_ground(k) {
                            None
                        } else {
                            r0.best
                                .get(k)
                                .copied()
                                .flatten()
                                .map(|b| self.recipes.tree(&r0, b, &HashSet::from([k.clone()])))
                        };
                        let opaque = node.is_none() && !have.contains(k) && !recipes::is_ground(k);
                        kids.push(Kid {
                            key: k.clone(),
                            name,
                            node,
                            opaque,
                        });
                    }
                }
                steps.push(ChainStep {
                    node: e.node,
                    verb: self.verb(world, e),
                    input: current_input,
                    output: e.out.clone(),
                    riders,
                    fails: e.fails,
                    maybe: e.maybe,
                });
                tree = Some(Tree { q, kids });
            }
            let Some(tree) = tree else { continue };
            let output = self.recipes.producer(q).out.clone();
            let mut path = Vec::new();
            if let Some(i) = node_of(&from) {
                path.push(i);
            }
            path.extend(steps.iter().map(|s| s.node));
            if let Some(i) = node_of(&output) {
                path.push(i);
            }
            let stops = road_stops(world, &path, &from);
            let from_words = recipes::key_words(world, &from);
            let out_words = recipes::key_words(world, &output);
            let verbs = steps
                .iter()
                .map(|s| s.verb.as_str())
                .collect::<Vec<_>>()
                .join(" › ");
            let brief = format!("from {from_words} › {verbs} → {out_words}");
            let mut rail = format!(
                "from your {from_words}{}",
                via.as_ref().map_or_else(String::new, |t| format!(
                    ", a {}",
                    t.trim_start_matches("any ")
                ))
            );
            for step in &steps {
                rail.push_str(&format!(
                    " › {}{}{}",
                    step.verb,
                    if step.fails { " or fails" } else { "" },
                    if step.maybe { " maybe" } else { "" }
                ));
                for rider in &step.riders {
                    rail.push_str(&format!(" + {rider}"));
                }
            }
            let code = recipes::chain_code(&self.recipes, world, &tree);
            chains.push(Chain {
                cost: c,
                from,
                output,
                via,
                steps,
                path,
                stops,
                brief,
                rail,
                code,
            });
            if chains.len() == CHAINS {
                break;
            }
        }
        chains
    }

    fn unified_score(
        &self,
        world: &World,
        q: usize,
        shape: &Shape,
        via: &[HashSet<String>],
        keys: &[String],
        permitted: &[Vec<bool>],
    ) -> Option<f64> {
        let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
        for j in 0..keys.len() {
            if let Some(variable) = self
                .requirements
                .get(&(q, j))
                .and_then(|r| r.variable.as_deref())
            {
                groups.entry(variable).or_default().push(j);
            }
        }
        let mut groups: Vec<_> = groups
            .into_iter()
            .filter(|(_, positions)| positions.len() > 1)
            .collect();
        groups.sort_by_key(|(variable, _)| *variable);
        if groups.is_empty() {
            return input_score(&shape.ins, via, keys, permitted);
        }
        let mut candidates: Vec<_> = shape
            .ins
            .iter()
            .flat_map(|input| input.keys.iter().cloned())
            .collect();
        candidates.sort_by_key(|key| {
            node_of(key).map_or_else(
                || key.clone(),
                |i| format!("{}::{}", world.qual(i), world.node(i).name),
            )
        });
        candidates.dedup();
        let mut masks = vec![permitted.to_vec()];
        for (_, positions) in groups {
            let mut next = Vec::new();
            for mask in masks {
                // None permits a group supplied entirely by extra arguments.
                for binding in candidates.iter().map(Some).chain(std::iter::once(None)) {
                    let mut bound = mask.clone();
                    for (a, input) in shape.ins.iter().enumerate() {
                        for &j in &positions {
                            bound[a][j] &= binding.is_some_and(|key| input.keys.contains(key));
                        }
                    }
                    next.push(bound);
                    if next.len() == 64 {
                        break;
                    }
                }
                if next.len() == 64 {
                    break;
                }
            }
            masks = next;
        }
        masks
            .iter()
            .filter_map(|mask| input_score(&shape.ins, via, keys, mask))
            .max_by(f64::total_cmp)
    }

    fn satisfies_key(&self, q: usize, j: usize, key: &str) -> bool {
        let Some(required) = self.requirements.get(&(q, j)) else {
            return true;
        };
        required.verified
            && (required.all.is_empty()
                || self
                    .capabilities
                    .get(key)
                    .is_some_and(|caps| required.all.iter().all(|cap| caps.contains(cap))))
    }

    fn satisfies(&self, q: usize, j: usize, keys: &HashSet<String>) -> bool {
        let Some(required) = self.requirements.get(&(q, j)) else {
            return true;
        };
        if !required.verified {
            return false;
        }
        if required.all.is_empty() {
            return true;
        }
        keys.iter().any(|key| {
            self.capabilities
                .get(key)
                .is_some_and(|caps| required.all.iter().all(|cap| caps.contains(cap)))
        })
    }

    fn hollow(
        &self,
        world: &World,
        k: &str,
        e: &Producer,
        best: &HashMap<String, (usize, usize)>,
    ) -> bool {
        if e.how != How::Method || e.ins.first().is_none_or(|first| first != k) {
            return false;
        }
        let Some(&(q, _)) = best.get(k) else {
            return false;
        };
        let made = self.recipes.producer(q);
        made.how == How::Variant
            || made
                .names
                .iter()
                .any(|name| name == world.node(e.node).name.as_ref())
    }

    fn verb(&self, world: &World, e: &Producer) -> String {
        match e.how {
            How::Variant => full_name(world, e.node),
            How::Literal => "build".to_owned(),
            How::Default => "default".to_owned(),
            How::Call | How::Method => world.node(e.node).name.to_string(),
        }
    }
}

/// Queries with arrows, or `takes` / `gives`, ask for a callable's shape.
#[must_use]
pub fn is_shape(query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.contains("->")
        || q.contains('→')
        || q.contains("=>")
        || q.starts_with("takes ")
        || q.starts_with("gives ")
}

fn node_of(key: &str) -> Option<NodeId> {
    key.strip_prefix('#')?.parse().ok()
}
fn full_name(world: &World, node: NodeId) -> String {
    let n = world.node(node);
    n.parent.map_or_else(
        || n.name.to_string(),
        |p| format!("{}::{}", world.node(p).name, n.name),
    )
}
fn identity(name: &str) -> bool {
    matches!(
        name,
        "as_ref"
            | "as_mut"
            | "borrow"
            | "borrow_mut"
            | "deref"
            | "deref_mut"
            | "clone"
            | "to_owned"
            | "into"
            | "index"
            | "index_mut"
            | "eq"
            | "ne"
            | "cmp"
            | "partial_cmp"
            | "lt"
            | "le"
            | "gt"
            | "ge"
            | "hash"
    )
}

fn road_stops(world: &World, path: &[NodeId], from: &str) -> Vec<RoadStop> {
    let mut stops: Vec<RoadStop> = Vec::new();
    for (a, &node) in path.iter().enumerate() {
        let n = world.node(node);
        let at = n.parent.unwrap_or(node);
        let yours = a == 0 && node_of(from) == Some(node);
        let call = (!yours && matches!(n.kind, Kind::Variant | Kind::Function | Kind::Method))
            .then_some(node);
        if let Some(prev) = stops.last_mut().filter(|s| s.node == at) {
            if let Some(call) = call {
                prev.calls.push(call);
            }
            continue;
        }
        stops.push(RoadStop {
            node: at,
            label: String::new(),
            calls: call.into_iter().collect(),
            yours,
        });
    }
    for stop in &mut stops {
        stop.label = world.node(stop.node).name.to_string();
        if !stop.calls.is_empty() && (stop.calls.len() > 1 || stop.calls[0] != stop.node) {
            stop.label.push_str(&format!(
                " · {}",
                stop.calls
                    .iter()
                    .map(|&i| world.node(i).name.as_ref())
                    .collect::<Vec<_>>()
                    .join(" › ")
            ));
        }
    }
    stops
}

fn input_score(
    ins: &[Words],
    via: &[HashSet<String>],
    keys: &[String],
    permitted: &[Vec<bool>],
) -> Option<f64> {
    if ins.len() > keys.len() {
        return None;
    }
    let weights: Vec<Vec<Option<f64>>> = ins
        .iter()
        .enumerate()
        .map(|(a, input)| {
            keys.iter()
                .enumerate()
                .map(|(j, key)| {
                    if !permitted[a][j] {
                        None
                    } else if input.keys.contains(key)
                        || (input.keys.contains("any") && key.starts_with("any "))
                    {
                        Some(2.0)
                    } else if via[a].contains(key) {
                        Some(1.0)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect();
    // Minimum-cost bipartite assignment gives the best concrete/trait mix.
    // Unlike greedy matching its score is invariant to query-input order.
    let mut owner = vec![0; keys.len() + 1];
    let mut previous = vec![0; keys.len() + 1];
    let mut row_potential = vec![0.0; ins.len() + 1];
    let mut column_potential = vec![0.0; keys.len() + 1];
    for wanted in 1..=ins.len() {
        owner[0] = wanted;
        let mut column = 0;
        let mut minimum = vec![f64::INFINITY; keys.len() + 1];
        let mut seen = vec![false; keys.len() + 1];
        loop {
            seen[column] = true;
            let row = owner[column];
            let mut delta = f64::INFINITY;
            let mut next = 0;
            for candidate in 1..=keys.len() {
                if seen[candidate] {
                    continue;
                }
                let cost = weights[row - 1][candidate - 1].map_or(1e9, |weight| 2.0 - weight);
                let reduced = cost - row_potential[row] - column_potential[candidate];
                if reduced < minimum[candidate] {
                    minimum[candidate] = reduced;
                    previous[candidate] = column;
                }
                if minimum[candidate] < delta {
                    delta = minimum[candidate];
                    next = candidate;
                }
            }
            if !delta.is_finite() {
                return None;
            }
            for candidate in 0..=keys.len() {
                if seen[candidate] {
                    row_potential[owner[candidate]] += delta;
                    column_potential[candidate] -= delta;
                } else {
                    minimum[candidate] -= delta;
                }
            }
            column = next;
            if owner[column] == 0 {
                break;
            }
        }
        loop {
            let next = previous[column];
            owner[column] = owner[next];
            column = next;
            if column == 0 {
                break;
            }
        }
    }
    let mut score = 0.0;
    for column in 1..=keys.len() {
        if owner[column] > 0 {
            score += weights[owner[column] - 1][column - 1]?;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    {
        Some(score - (keys.len() - ins.len()) as f64 * 0.6)
    }
}

#[derive(PartialEq)]
struct Queued(f64, String);
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .0
            .total_cmp(&self.0)
            .then_with(|| other.1.cmp(&self.1))
    }
}

#[cfg(test)]
mod tests;
