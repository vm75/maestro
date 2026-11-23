use std::mem::MaybeUninit;

use anyhow::{Result, bail, ensure};

struct Parser(Box<MaybeUninit<unsafe_libyaml::yaml_parser_t>>);

impl Drop for Parser {
    fn drop(&mut self) {
        // Parser is constructed only after initialization succeeds.
        unsafe { unsafe_libyaml::yaml_parser_delete(self.0.as_mut_ptr()) };
    }
}

pub(super) fn check(input: &[u8]) -> Result<()> {
    let mut raw = Box::new(MaybeUninit::uninit());
    // libyaml owns its internal allocations. The borrowed input stays alive
    // and immovable throughout the synchronous event loop.
    unsafe {
        ensure!(
            unsafe_libyaml::yaml_parser_initialize(raw.as_mut_ptr()).ok,
            "could not initialize YAML parser"
        );
        let mut parser = Parser(raw);
        unsafe_libyaml::yaml_parser_set_input_string(
            parser.0.as_mut_ptr(),
            input.as_ptr(),
            input.len() as u64,
        );
        let mut documents = 0;
        let mut depth = 0;
        let mut nodes = 0;
        loop {
            let mut event = MaybeUninit::uninit();
            if unsafe_libyaml::yaml_parser_parse(parser.0.as_mut_ptr(), event.as_mut_ptr()).fail {
                bail!("invalid YAML configuration");
            }
            let mut event = event.assume_init();
            let kind = event.type_;
            let (anchor, tag) = match kind {
                unsafe_libyaml::YAML_SCALAR_EVENT => {
                    (event.data.scalar.anchor, event.data.scalar.tag)
                }
                unsafe_libyaml::YAML_MAPPING_START_EVENT => (
                    event.data.mapping_start.anchor,
                    event.data.mapping_start.tag,
                ),
                unsafe_libyaml::YAML_SEQUENCE_START_EVENT => (
                    event.data.sequence_start.anchor,
                    event.data.sequence_start.tag,
                ),
                _ => (std::ptr::null_mut(), std::ptr::null_mut()),
            };
            let forbidden =
                !anchor.is_null() || !tag.is_null() || kind == unsafe_libyaml::YAML_ALIAS_EVENT;
            unsafe_libyaml::yaml_event_delete(&mut event);
            ensure!(!forbidden, "unsupported YAML anchor, alias or explicit tag");
            match kind {
                unsafe_libyaml::YAML_DOCUMENT_START_EVENT => documents += 1,
                unsafe_libyaml::YAML_MAPPING_START_EVENT
                | unsafe_libyaml::YAML_SEQUENCE_START_EVENT => {
                    depth += 1;
                    nodes += 1;
                }
                unsafe_libyaml::YAML_SCALAR_EVENT => nodes += 1,
                unsafe_libyaml::YAML_MAPPING_END_EVENT
                | unsafe_libyaml::YAML_SEQUENCE_END_EVENT => depth -= 1,
                unsafe_libyaml::YAML_STREAM_END_EVENT => break,
                _ => {}
            }
            ensure!(
                documents <= 1 && depth <= 128 && nodes <= 1 << 20,
                "YAML structure exceeds its limit"
            );
        }
        ensure!(
            documents == 1,
            "configuration must contain one YAML document"
        );
    }
    Ok(())
}
