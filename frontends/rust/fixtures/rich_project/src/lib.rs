#![allow(dead_code)]

macro_rules! make_answer {
    ($name:ident) => {
        fn $name() -> i32 {
            42
        }
    };
}

pub struct Boxed {
    pub value: i32,
}

impl Boxed {
    pub fn get(&self) -> i32 {
        self.value
    }
}

pub enum Choice {
    Yes,
    No,
}

pub trait Marker {}

impl Marker for Boxed {}

pub type Alias = Boxed;

pub fn compute() -> i32 {
    let item = Boxed { value: 1 };
    item.get()
}

make_answer!(generated);
