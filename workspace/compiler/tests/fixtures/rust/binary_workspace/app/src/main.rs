fn main() {
    let counter = corelib::CoreCounter::new(1);
    println!("{}", counter.value());
}
