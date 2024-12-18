pub struct BytesDebug<'a>(pub &'a [u8]);

impl<'a> std::fmt::Debug for BytesDebug<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("0x")?;
        for byte in self.0 {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}
