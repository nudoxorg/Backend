pub struct CoreCounter {
    value: i32,
}

impl CoreCounter {
    pub fn new(value: i32) -> Self { Self { value } }

    pub fn value(&self) -> i32 { self.value }
}
