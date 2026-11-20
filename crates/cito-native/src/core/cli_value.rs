//! Value framing shared by CLI command families. Callers own field validation.
pub(super) fn read<'a>(
    args: &'a [String],
    index: &mut usize,
    flag: &str,
    allow_empty: bool,
) -> Result<&'a str, String> {
    let value = if let Some((_, value)) = args[*index].split_once('=') {
        value
    } else {
        *index += 1;
        args.get(*index)
            .filter(|value| !value.starts_with('-'))
            .map(String::as_str)
            .ok_or_else(|| format!("Missing value for {flag}"))?
    };
    if value.is_empty() && !allow_empty {
        return Err(format!("Missing value for {flag}"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inline_values_preserve_leading_hyphens_and_empty_policy() {
        let args = ["--message=-value".into()];
        assert_eq!(read(&args, &mut 0, "--message", false).unwrap(), "-value");
        let args = ["--message".into(), "--other".into()];
        assert!(read(&args, &mut 0, "--message", false).is_err());
        let args = ["--issuer=".into()];
        assert!(read(&args, &mut 0, "--issuer", false).is_err());
        assert_eq!(read(&args, &mut 0, "--issuer", true).unwrap(), "");
    }
}
