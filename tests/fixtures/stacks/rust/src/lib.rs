/// Formats an integer with the itoa crate.
pub fn render(value: u64) -> String {
    itoa::Buffer::new().format(value).to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_with_the_dependency() {
        assert_eq!(super::render(1234), "1234");
    }
}
