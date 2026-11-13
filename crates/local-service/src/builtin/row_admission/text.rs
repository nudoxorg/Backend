//! Exact, fallible UTF-8 storage construction after prospective admission.

use std::fmt::{self, Write};

/// A measured formatting operation with an exact requested byte layout.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ExactText {
    length: usize,
}

impl ExactText {
    pub(crate) fn measure(arguments: fmt::Arguments<'_>) -> Result<Self, &'static str> {
        let mut counter = Counter(0);
        counter
            .write_fmt(arguments)
            .map_err(|_| "row text length overflow or formatting failure")?;
        if counter.0 > isize::MAX as usize {
            return Err("row text layout overflow");
        }
        Ok(Self { length: counter.0 })
    }

    pub(crate) const fn len(self) -> usize {
        self.length
    }

    /// Allocates exactly the measured byte layout. The caller first charges
    /// `len()` to its requested-layout quota. No growth capacity is inferred.
    pub(crate) fn allocate(self, arguments: fmt::Arguments<'_>) -> Result<String, &'static str> {
        // Audited bytemuck 1.25.2 uses Layout::array::<u8>(length) and
        // alloc_zeroed directly. Box<[u8]>::into_vec preserves that layout;
        // String::from_utf8 takes over the same allocation on valid UTF-8.
        let mut bytes = bytemuck::allocation::try_zeroed_slice_box::<u8>(self.length)
            .map_err(|()| "row text allocation failed")?;
        let mut writer = Buffer {
            remaining: &mut bytes,
            written: 0,
        };
        writer
            .write_fmt(arguments)
            .map_err(|_| "row text changed after admission")?;
        if writer.written != self.length {
            return Err("row text changed after admission");
        }
        String::from_utf8(bytes.into_vec()).map_err(|_| "row text formatter emitted invalid UTF-8")
    }
}

struct Counter(usize);
impl Write for Counter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0 = self.0.checked_add(text.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

struct Buffer<'a> {
    remaining: &'a mut [u8],
    written: usize,
}
impl Write for Buffer<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let (destination, remaining) = std::mem::take(&mut self.remaining)
            .split_at_mut_checked(text.len())
            .ok_or(fmt::Error)?;
        destination.copy_from_slice(text.as_bytes());
        self.remaining = remaining;
        self.written += text.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_and_maximum_ordinal_preserve_exact_capacity() {
        let expected = "pkg::migrations/α.sql::book\0sql:4294967295";
        let plan = ExactText::measure(format_args!(
            "pkg::migrations/α.sql::book\0sql:{}",
            u32::MAX
        ))
        .expect("measure");
        let text = plan
            .allocate(format_args!(
                "pkg::migrations/α.sql::book\0sql:{}",
                u32::MAX
            ))
            .expect("allocate");
        assert_eq!(text, expected);
        assert_eq!(text.len(), plan.len());
        assert_eq!(text.capacity(), plan.len());
    }

    #[test]
    fn formatter_cannot_exceed_or_underfill_admitted_layout() {
        let plan = ExactText::measure(format_args!("123")).expect("measure");
        assert!(plan.allocate(format_args!("1234")).is_err());
        assert!(plan.allocate(format_args!("12")).is_err());
    }

    #[test]
    fn empty_text_has_no_requested_heap_capacity() {
        let plan = ExactText::measure(format_args!("")).expect("measure");
        let text = plan.allocate(format_args!("")).expect("allocate");
        assert_eq!(text.capacity(), 0);
    }
}
