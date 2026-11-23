use std::mem::MaybeUninit;

use anyhow::{Result, ensure};

struct Parser(Box<MaybeUninit<unsafe_libyaml::yaml_parser_t>>);

impl Drop for Parser {
    fn drop(&mut self) {
        // Parser is constructed only after initialization succeeds.
        unsafe { unsafe_libyaml::yaml_parser_delete(self.0.as_mut_ptr()) };
    }
}

pub(super) fn check(input: &[u8]) -> Result<()> {
    crate::config_yaml::check(input)
}

struct Document(Box<MaybeUninit<unsafe_libyaml::yaml_document_t>>);
impl Drop for Document {
    fn drop(&mut self) {
        unsafe { unsafe_libyaml::yaml_document_delete(self.0.as_mut_ptr()) };
    }
}

/// Insert credentials using parser byte spans, preserving authored YAML outside
/// the two credential scalars. No line-based guessing of mapping boundaries.
pub(super) fn with_admin(input: &[u8], username: &str, hash: &str) -> Result<Vec<u8>> {
    ensure!(
        input.len() <= super::MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    // libyaml's encoding detection consumes the leading UTF-8 BOM without
    // including it in source marks. Keep it outside the indexed input.
    let original = input;
    let mut input = input;
    while let Some(rest) = input.strip_prefix(b"\xef\xbb\xbf") {
        input = rest;
    }
    let prefix = &original[..original.len() - input.len()];
    ensure!(
        super::server::valid_username(username) && super::server::valid_password_hash(hash),
        "invalid administrator credentials"
    );
    super::server::read(
        input,
        &super::server::Settings::defaults(std::path::Path::new("/"), std::path::Path::new("/")),
    )?;
    let mut raw = Box::new(MaybeUninit::uninit());
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
        let mut raw_document = Box::new(MaybeUninit::uninit());
        ensure!(
            unsafe_libyaml::yaml_parser_load(parser.0.as_mut_ptr(), raw_document.as_mut_ptr()).ok,
            "could not inspect YAML document"
        );
        let mut document = Document(raw_document);
        let root = unsafe_libyaml::yaml_document_get_root_node(document.0.as_mut_ptr());
        ensure!(
            !root.is_null() && (*root).type_ == unsafe_libyaml::YAML_MAPPING_NODE,
            "configuration must be a mapping"
        );
        let fields = [("admin_username", username), ("admin_password_hash", hash)];
        let root_pairs = mapping_pairs(&mut document, root);
        let server = root_pairs
            .iter()
            .find(|(name, _)| name == "server")
            .map(|(_, node)| *node);
        let newline = if input.windows(2).any(|bytes| bytes == b"\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut patches = Vec::new();
        if let Some(server) = server {
            if (*server).type_ == unsafe_libyaml::YAML_MAPPING_NODE {
                let pairs = mapping_pairs(&mut document, server);
                let mut missing = Vec::new();
                for (name, value) in fields {
                    if let Some((_, scalar)) = pairs.iter().find(|(key, _)| key == name) {
                        ensure!(
                            (**scalar).type_ == unsafe_libyaml::YAML_SCALAR_NODE,
                            "administrator fields must be scalars"
                        );
                        let start = (**scalar).start_mark.index as usize;
                        let mut end = (**scalar).end_mark.index as usize;
                        if matches!(
                            (**scalar).data.scalar.style,
                            unsafe_libyaml::YAML_LITERAL_SCALAR_STYLE
                                | unsafe_libyaml::YAML_FOLDED_SCALAR_STYLE
                        ) {
                            // Empty block scalars may carry a header comment.
                            // Replace only the style indicator, retaining the
                            // comment and authored blank lines after it.
                            end = start
                                + input[start..end]
                                    .iter()
                                    .position(|byte| byte.is_ascii_whitespace() || *byte == b'#')
                                    .unwrap_or(end - start);
                        }
                        patches.push((start, end, format!("'{value}'")));
                    } else {
                        missing.push(format!("{name}: '{value}'"));
                    }
                }
                if !missing.is_empty() {
                    insert_mapping(&mut patches, server, &missing, pairs.is_empty(), newline);
                }
            } else {
                // The caller has validated that a non-mapping server is null.
                let value =
                    format!(" {{admin_username: '{username}', admin_password_hash: '{hash}'}}");
                patches.push((
                    (*server).start_mark.index as usize,
                    (*server).end_mark.index as usize,
                    value,
                ));
            }
        } else {
            let value =
                format!("server: {{admin_username: '{username}', admin_password_hash: '{hash}'}}");
            insert_mapping(&mut patches, root, &[value], root_pairs.is_empty(), newline);
        }
        patches.sort_by_key(|(start, end, _)| (*start, *end));
        let mut output = prefix.to_vec();
        let mut cursor = 0;
        for (start, end, replacement) in patches {
            ensure!(
                cursor <= start && start <= end && end <= input.len(),
                "invalid YAML edit span"
            );
            output.extend_from_slice(&input[cursor..start]);
            output.extend_from_slice(replacement.as_bytes());
            cursor = end;
        }
        output.extend_from_slice(&input[cursor..]);
        Ok(output)
    }
}

unsafe fn mapping_pairs(
    document: &mut Document,
    node: *mut unsafe_libyaml::yaml_node_t,
) -> Vec<(String, *mut unsafe_libyaml::yaml_node_t)> {
    unsafe {
        let pairs = (*node).data.mapping.pairs;
        if pairs.top == pairs.start {
            return Vec::new();
        }
        let count = pairs.top.offset_from(pairs.start) as usize;
        if count == 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts(pairs.start, count)
            .iter()
            .map(|pair| {
                let key = unsafe_libyaml::yaml_document_get_node(document.0.as_mut_ptr(), pair.key);
                let value =
                    unsafe_libyaml::yaml_document_get_node(document.0.as_mut_ptr(), pair.value);
                let name = std::slice::from_raw_parts(
                    (*key).data.scalar.value,
                    (*key).data.scalar.length as usize,
                );
                (String::from_utf8_lossy(name).into_owned(), value)
            })
            .collect()
    }
}

unsafe fn insert_mapping(
    patches: &mut Vec<(usize, usize, String)>,
    node: *mut unsafe_libyaml::yaml_node_t,
    fields: &[String],
    empty: bool,
    newline: &str,
) {
    unsafe {
        let flow = (*node).data.mapping.style == unsafe_libyaml::YAML_FLOW_MAPPING_STYLE;
        let position = (*node).start_mark.index as usize + usize::from(flow);
        let text = if flow {
            format!("{}{}", fields.join(", "), if empty { "" } else { ", " })
        } else {
            let indentation = " ".repeat((*node).start_mark.column as usize);
            format!(
                "{}{}{}",
                fields.join(&format!("{newline}{indentation}")),
                newline,
                indentation
            )
        };
        patches.push((position, position, text));
    }
}

#[cfg(test)]
mod admin_tests {
    use super::*;
    const HASH: &str =
        "pbkdf2-sha256$600000$AAECAwQFBgcICQoLDA0ODw$qXOZvDy01GeMq44VItTr5/qfaPfFVEtkY1IyHndgOno";
    #[test]
    fn credential_patches_preserve_authored_comments_and_resources() {
        let resources = "services: {broken: {unknown: repair-me}} # resource comment";
        for settings in [
            "",
            "server: {} # empty settings\n",
            "server: null # null settings\n",
            "server: # implicit null\n",
            "server: {listen: '127.0.0.1:8181'} # flow settings\n",
            "server:\n  # réglages\n  listen: '127.0.0.1:8181' # listener\n",
            "server:\n  admin_username: '' # username\n  admin_password_hash: '' # hash\n",
            "server:\n  admin_username: |- # username\n\n  admin_password_hash: >- # hash\n\n",
            "server: {admin_username: '', admin_password_hash: ''} # placeholders\n",
        ] {
            for newline in ["\n", "\r\n"] {
                let original = format!("# café 🦀\nversion: 1 # version\n{settings}{resources}\n")
                    .replace('\n', newline);
                let result = with_admin(original.as_bytes(), "Admin", HASH).unwrap();
                let result = std::str::from_utf8(&result).unwrap();
                let parsed = super::super::server::read(
                    result.as_bytes(),
                    &super::super::server::Settings::defaults(
                        std::path::Path::new("/"),
                        std::path::Path::new("/"),
                    ),
                )
                .unwrap()
                .unwrap();
                assert_eq!(parsed.admin_username, "Admin");
                assert_eq!(parsed.admin_password_hash, HASH);
                assert!(result.contains(resources));
                for comment in original
                    .lines()
                    .filter_map(|line| line.split_once('#').map(|(_, comment)| comment))
                {
                    assert!(
                        result.contains(&format!("#{comment}")),
                        "lost comment in {settings}"
                    );
                }
                if newline == "\r\n" {
                    assert!(!result.replace("\r\n", "").contains('\n'));
                }
            }
        }
    }

    #[test]
    fn credentials_can_be_inserted_into_a_flow_root_without_reformatting_it() {
        for input in [
            "{version: 1, services: {broken: {unknown: 'é'}}} # root\n",
            "{version: 1, server: null, services: {}} # root\n",
            "\u{feff}# UTF-8 BOM\nversion: 1\nserver: {}\n",
        ] {
            let result = with_admin(input.as_bytes(), "Admin", HASH).unwrap();
            let text = String::from_utf8(result).unwrap();
            assert!(text.contains("# root") || text.contains("# UTF-8 BOM"));
            let parsed = super::super::server::read(
                text.as_bytes(),
                &super::super::server::Settings::defaults(
                    std::path::Path::new("/"),
                    std::path::Path::new("/"),
                ),
            )
            .unwrap_or_else(|error| panic!("{error}: {text:?}"))
            .unwrap();
            assert_eq!(parsed.admin_username, "Admin");
        }
    }
}
