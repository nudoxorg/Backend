//! The grammar on real declarations: the shapes toml, serde_json and tokio
//! actually spell, in one line and across several, in every ecosystem.

#![allow(clippy::expect_used, clippy::panic)]

use super::{Glyph, Ink, Item, Shape, normalize, read};
use crate::icons::{Kind, Lang};

fn rust(name: &str, signature: &str) -> super::Reading {
    read(&Item::new(name, Lang::Rust).signature(Some(signature)))
}

fn words(name: &str, signature: &str) -> Vec<String> {
    rust(name, signature)
        .badges
        .iter()
        .map(|b| b.word.to_string())
        .collect()
}

#[test]
fn a_fallible_generic_function_reads_takes_generic_can_fail() {
    // toml::from_str
    let reading = rust(
        "from_str",
        "pub fn from_str<T>(s: &str) -> Result<T, Error> where T: DeserializeOwned",
    );
    assert_eq!(reading.shape, Shape::Function);
    assert_eq!(reading.word, "fn");
    assert_eq!(reading.words(), ["takes 1", "T", "can fail"]);
    let fail = reading
        .badges
        .iter()
        .find(|b| b.word == "can fail")
        .expect("a can-fail badge");
    assert_eq!(fail.ink, Ink::Coral);
    assert_eq!(fail.glyph, Glyph::Fail);
    assert!(
        fail.tip.contains("Result"),
        "the tip says why: {}",
        fail.tip
    );
    // Nothing of the source survives into a word.
    for badge in &reading.badges {
        assert!(
            !badge.word.contains("pub") && !badge.word.contains("fn "),
            "{}",
            badge.word
        );
    }
}

#[test]
fn receivers_say_whether_the_method_reads_changes_or_consumes() {
    assert_eq!(
        words(
            "get",
            "pub fn get<Q>(&self, key: &Q) -> Option<&Value> where Q: ?Sized + Ord, String: Borrow<Q>"
        ),
        ["reads it", "takes 1", "Q", "maybe"]
    );
    assert_eq!(
        words(
            "insert",
            "pub fn insert(&mut self, k: String, v: Value) -> Option<Value>"
        ),
        ["changes it", "takes 2", "maybe"]
    );
    assert_eq!(
        words("into_mut", "pub fn into_mut(self) -> &'a mut Value"),
        ["consumes it"]
    );
    assert_eq!(
        words("len", "pub const fn len(&self) -> usize"),
        ["const", "reads it"]
    );
    assert_eq!(
        words("iter", "pub fn iter(&self) -> Iter<'_>"),
        ["reads it", "iterates"]
    );
    assert_eq!(
        words("new", "pub fn new() -> Self"),
        ["takes nothing", "makes one"]
    );
    assert_eq!(
        words("is_null", "pub fn is_null(&self) -> bool"),
        ["reads it", "yes / no"]
    );
}

#[test]
fn arguments_borrowed_mutably_are_a_write() {
    let reading = rust(
        "read_into",
        "pub fn read_into(reader: &mut R, out: &mut Vec<u8>) -> io::Result<usize>",
    );
    assert!(reading.says("writes into"), "{:?}", reading.words());
    assert!(reading.says("can fail"), "io::Result is a Result");
}

#[test]
fn qualifiers_earn_their_own_badges() {
    let unsafe_fn = rust(
        "from_utf8_unchecked",
        "pub unsafe fn from_utf8_unchecked(v: &[u8]) -> &str",
    );
    assert_eq!(unsafe_fn.words(), ["unsafe", "takes 1"]);
    assert_eq!(unsafe_fn.badges[0].ink, Ink::Amber);
    assert_eq!(
        words("sleep", "pub async fn sleep(duration: Duration)"),
        ["async", "takes 1"]
    );
    assert!(rust("callback", "pub unsafe extern \"C\" fn callback(x: i32)").says("C ABI"));
    assert_eq!(
        words("wait", "pub fn wait(&self) -> impl Future<Output = ()>"),
        ["reads it", "future"]
    );
}

#[test]
fn an_arrow_inside_a_bound_never_closes_the_generic_list() {
    assert_eq!(
        words(
            "map",
            "pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Option<U>"
        ),
        ["consumes it", "takes 1", "U F", "maybe"]
    );
    assert_eq!(
        words(
            "retain",
            "pub fn retain<F>(&mut self, mut keep: F) where F: FnMut(&String, &mut Value) -> bool"
        ),
        ["changes it", "takes 1", "F"],
        "the `-> bool` sits in a bound, not the return type"
    );
}

#[test]
fn a_signature_across_several_lines_reads_like_one() {
    let source = "pub fn deserialize<'de, D>(\n    deserializer: D,\n) -> Result<Self, D::Error>\nwhere\n    D: Deserializer<'de>,";
    assert_eq!(words("deserialize", source), ["takes 1", "D", "can fail"]);
    assert_eq!(
        normalize("#[inline]\npub fn f(\n  a: u8,\n)"),
        "pub fn f( a: u8, )"
    );
}

#[test]
fn types_read_their_generics_borrows_and_roles() {
    // tokio::sync::mpsc, as the board's card shows them
    assert_eq!(words("Sender", "pub struct Sender<T> {"), ["T"]);
    assert_eq!(
        words("Permit", "pub struct Permit<'a, T> {"),
        ["borrows", "T"]
    );
    assert_eq!(
        words("PermitIterator", "pub struct PermitIterator<'a, T> {"),
        ["borrows", "T", "iterator"]
    );
    assert_eq!(
        words("SendError", "pub struct SendError<T>(pub T);"),
        ["T", "wraps T", "error"]
    );
    assert_eq!(
        words("TrySendError", "pub enum TrySendError<T> {"),
        ["T", "error"]
    );
    assert_eq!(
        words("RecvError", "pub struct RecvError;"),
        ["marker", "error"]
    );
    assert_eq!(words("Value", "pub enum Value {"), Vec::<String>::new());
    assert_eq!(words("Deserializer", "pub struct Deserializer<R> {"), ["R"]);
    assert_eq!(
        words("MutexGuard", "pub struct MutexGuard<'a, T: ?Sized> {"),
        ["borrows", "T", "guard"]
    );
    assert_eq!(words("Builder", "pub struct Builder {"), ["builder"]);
    assert_eq!(words("Wrapper", "pub struct Wrapper(u32);"), ["wraps u32"]);
    assert_eq!(rust("Value", "pub enum Value {").word, "enum");
    assert_eq!(
        rust("Sender", "pub struct Sender<T> {").shape,
        Shape::Struct
    );
}

#[test]
fn contracts_aliases_values_and_macros() {
    assert_eq!(
        words("Index", "pub trait Index: private::Sealed {"),
        ["needs Sealed"]
    );
    assert_eq!(
        words("Visitor", "pub trait Visitor<'de>: Sized {"),
        ["needs Sized"]
    );
    assert_eq!(
        words(
            "Serializer",
            "pub trait Serializer<T>: Clone + Send + 'static {"
        ),
        ["T", "needs Clone", "needs Send"]
    );
    assert_eq!(words("Sealed", "pub unsafe trait Sealed {"), ["unsafe"]);
    assert_eq!(
        rust("Index", "pub trait Index: private::Sealed {").word,
        "trait"
    );
    assert_eq!(
        words("Result", "pub type Result<T> = result::Result<T, Error>;"),
        ["is Result"]
    );
    assert_eq!(
        rust("Result", "pub type Result<T> = result::Result<T, Error>;").shape,
        Shape::Alias
    );
    assert_eq!(
        words("MAX", "pub const MAX_BUFFER: usize = 1 << 20;"),
        ["usize"]
    );
    assert_eq!(
        words("COUNTER", "pub static mut COUNTER: u32 = 0;"),
        ["mutable static", "u32"]
    );
    assert_eq!(
        words("NAME", "pub const NAME: &'static str = \"x\";"),
        ["&str"]
    );
    let select = rust("select", "macro_rules! select {");
    assert_eq!(select.words(), ["select!"]);
    assert_eq!(select.shape, Shape::Macro);
}

#[test]
fn visibility_qualifiers_do_not_change_the_reading() {
    assert_eq!(
        words("f", "pub(crate) fn f(a: u8) -> bool"),
        words("f", "fn f(a: u8) -> bool")
    );
    assert_eq!(words("f", "pub(in crate::x) fn f()"), ["takes nothing"]);
}

#[test]
fn a_function_pointer_type_is_not_a_function() {
    let reading = rust("HANDLER", "pub const HANDLER: fn(u8) -> u8 = h;");
    assert_eq!(reading.shape, Shape::Constant);
    assert!(!reading.says("takes 1"));
}

#[test]
fn a_signature_that_is_not_source_reads_to_no_badges_but_keeps_its_kind() {
    let item = Item::new("Thing", Lang::Rust)
        .kind(Some(Kind::Struct))
        .signature(Some("nominal(entity=Thing, package=x)"));
    let reading = read(&item);
    assert!(reading.badges.is_empty());
    assert_eq!(reading.shape, Shape::Struct);
    assert_eq!(reading.word, "struct");
    // No signature at all: the kind still says what it is.
    let bare = read(&Item::new("go", Lang::Rust).kind(Some(Kind::Function)));
    assert_eq!(
        (bare.shape, bare.word, bare.badges.len()),
        (Shape::Function, "fn", 0)
    );
}

#[test]
fn typescript_reads_zod_shapes() {
    let ts = |name: &str, sig: &str, kind: Option<Kind>| {
        read(
            &Item::new(name, Lang::Typescript)
                .kind(kind)
                .signature(Some(sig)),
        )
    };
    let parse = ts(
        "parseAsync",
        "export async function parseAsync<T>(schema: ZodType<T>, data: unknown): Promise<T>",
        None,
    );
    assert_eq!(parse.words(), ["async", "takes 2", "T"]);
    assert_eq!(parse.word, "function");
    let string = ts(
        "string",
        "export function string(params?: RawCreateParams): ZodString",
        None,
    );
    assert_eq!(string.words(), ["takes 1"]);
    let error = ts(
        "ZodError",
        "export class ZodError<T = any> extends Error",
        None,
    );
    assert_eq!(error.words(), ["T", "extends Error"]);
    assert_eq!(error.word, "class");
    let def = ts("ZodTypeDef", "export interface ZodTypeDef", None);
    assert_eq!((def.shape, def.word), (Shape::Contract, "interface"));
    let find = ts(
        "find",
        "export function find(xs: string[]): string | undefined",
        None,
    );
    assert!(find.says("maybe"));
    let arrow = ts("map", "export const map = async (x: number) => x", None);
    assert!(arrow.says("async"));
}

#[test]
fn go_reads_pflag_shapes() {
    let go = |name: &str, sig: &str| read(&Item::new(name, Lang::Go).signature(Some(sig)));
    let parse = go("Parse", "func (f *FlagSet) Parse(arguments []string) error");
    assert_eq!(parse.words(), ["changes it", "takes 1", "can fail"]);
    assert_eq!(parse.word, "func");
    assert_eq!(
        go(
            "BoolVar",
            "func BoolVar(p *bool, name string, value bool, usage string)"
        )
        .words(),
        ["takes 4"]
    );
    assert_eq!(
        go(
            "GetBool",
            "func (f *FlagSet) GetBool(name string) (bool, error)"
        )
        .words(),
        ["changes it", "takes 1", "can fail"]
    );
    assert_eq!(go("Value", "type Value interface").shape, Shape::Contract);
    assert_eq!(go("FlagSet", "type FlagSet struct").shape, Shape::Struct);
    assert_eq!(
        go("Len", "func (s Set) Len() int").words(),
        ["reads it", "takes nothing"]
    );
}

#[test]
fn python_java_csharp_and_cpp_read_what_the_text_says() {
    let py = |sig: &str| read(&Item::new("f", Lang::Python).signature(Some(sig)));
    assert_eq!(
        py("async def fetch(session, url: str) -> Optional[bytes]:").words(),
        ["async", "takes 2", "maybe"]
    );
    assert_eq!(
        py("def parse(self, text: str) -> Iterator[Token]:").words(),
        ["method", "takes 1", "iterates"]
    );
    assert_eq!(py("class Reader(Protocol):").shape, Shape::Contract);
    let java = |sig: &str| read(&Item::new("copyOf", Lang::Java).signature(Some(sig)));
    assert_eq!(
        java("public static <T> List<T> copyOf(Collection<T> c) throws IOException").words(),
        ["static", "takes 1", "T", "can fail"]
    );
    assert_eq!(java("public interface Visitor<R>").shape, Shape::Contract);
    let cs = |sig: &str| read(&Item::new("CountAsync", Lang::Csharp).signature(Some(sig)));
    assert_eq!(
        cs("public static async Task<int> CountAsync<T>(IEnumerable<T> src)").words(),
        ["static", "async", "takes 1", "T"]
    );
    let cpp = |sig: &str| read(&Item::new("find", Lang::Cpp).signature(Some(sig)));
    assert_eq!(
        cpp("template <typename T> std::optional<T> find(const std::vector<T>& v) const noexcept")
            .words(),
        ["noexcept", "reads it", "takes 1", "T", "maybe"]
    );
}

#[test]
fn every_glyph_has_a_distinct_name() {
    let mut names: Vec<&str> = Glyph::ALL.iter().map(|glyph| glyph.name()).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(names.len(), before);
}
