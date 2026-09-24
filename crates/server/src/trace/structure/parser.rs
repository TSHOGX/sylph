//! Validated lexical spans, without building a JSON tree or allocating scalar bodies.
use std::io::Read;
use struson::reader::{JsonReader, JsonStreamReader, ReaderSettings, ValueType};

pub(crate) use sylph_protocol::trace::StructureKind as Kind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Node {
    pub id: u64,
    pub parent: Option<u64>,
    pub ordinal: u64,
    pub kind: Kind,
    pub start: u64,
    pub name: Option<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Start(Node),
    End { id: u64, end: u64, children: u64 },
}

struct Container {
    id: u64,
    kind: Kind,
    children: u64,
}

/// Struson 0.7.2's `skip_name` and scalar `skip_value` use streaming byte
/// collectors only when path tracking is disabled. `next_name_owned`, `next_str`
/// and serde Value were rejected: each can allocate a body-sized string. The
/// reader itself validates UTF-8, escapes, number grammar and nesting; this code
/// only records its positions and keeps a bounded stack of open containers.
pub(crate) fn scan(
    source: impl Read,
    max_nodes: u64,
    max_depth: u32,
    mut emit: impl FnMut(Event) -> anyhow::Result<()>,
) -> anyhow::Result<u64> {
    anyhow::ensure!(
        max_nodes > 0 && max_depth > 0,
        "Structural limits must be positive"
    );
    let mut reader = JsonStreamReader::new_custom(
        source,
        ReaderSettings {
            track_path: false,
            restrict_number_values: false,
            max_nesting_depth: Some(max_depth),
            ..Default::default()
        },
    );
    let mut stack: Vec<Container> = Vec::new();
    let mut count = 0;
    loop {
        let mut name = None;
        if let Some(container) = stack.last() {
            if !reader.has_next()? {
                let container = stack.pop().unwrap();
                match container.kind {
                    Kind::Object => reader.end_object()?,
                    Kind::Array => reader.end_array()?,
                    _ => unreachable!(),
                }
                emit(Event::End {
                    id: container.id,
                    end: position(&reader),
                    children: container.children,
                })?;
                if stack.is_empty() {
                    break;
                }
                continue;
            }
            if container.kind == Kind::Object {
                let start = position(&reader);
                reader.skip_name()?;
                name = Some(Span {
                    start,
                    end: position(&reader),
                });
            }
        }
        let value_type = reader.peek()?;
        let kind = match value_type {
            ValueType::Object => Kind::Object,
            ValueType::Array => Kind::Array,
            ValueType::String => Kind::String,
            ValueType::Number => Kind::Number,
            ValueType::Boolean => Kind::Boolean,
            ValueType::Null => Kind::Null,
        };
        anyhow::ensure!(count < max_nodes, "Structured node limit exceeded");
        count += 1;
        let id = count;
        let (parent, ordinal) = match stack.last_mut() {
            Some(container) => {
                let ordinal = container.children;
                container.children += 1;
                (Some(container.id), ordinal)
            }
            None => (None, 0),
        };
        emit(Event::Start(Node {
            id,
            parent,
            ordinal,
            kind,
            start: position(&reader),
            name,
        }))?;
        match kind {
            Kind::Object | Kind::Array => {
                match kind {
                    Kind::Object => reader.begin_object()?,
                    _ => reader.begin_array()?,
                }
                stack.push(Container {
                    id,
                    kind,
                    children: 0,
                });
            }
            _ => {
                reader.skip_value()?;
                emit(Event::End {
                    id,
                    end: position(&reader),
                    children: 0,
                })?;
                if stack.is_empty() {
                    break;
                }
            }
        }
    }
    reader.consume_trailing_whitespace()?;
    Ok(count)
}

fn position(reader: &impl JsonReader) -> u64 {
    reader
        .current_position(false)
        .data_pos
        .expect("JsonStreamReader reports byte coordinates")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};

    struct Tiny<R>(R);
    impl<R: Read> Read for Tiny<R> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let n = output.len().min(1);
            self.0.read(&mut output[..n])
        }
    }

    #[test]
    fn duplicate_members_and_lexical_numbers_retain_distinct_exact_spans() {
        let bytes = br#" { "x" : [ 1e4000, -0.00, "\uD83D\uDE00" ], "x":true, "": null } "#;
        let mut nodes = std::collections::BTreeMap::new();
        let count = scan(Tiny(Cursor::new(bytes)), 100, 32, |event| {
            match event {
                Event::Start(node) => {
                    nodes.insert(node.id, (node, 0, 0));
                }
                Event::End { id, end, children } => {
                    let node = nodes.get_mut(&id).unwrap();
                    node.1 = end;
                    node.2 = children;
                }
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 7);
        let values: Vec<_> = nodes
            .values()
            .map(|(node, end, _)| {
                std::str::from_utf8(&bytes[node.start as usize..*end as usize]).unwrap()
            })
            .collect();
        assert_eq!(
            &values[2..],
            &["1e4000", "-0.00", r#""\uD83D\uDE00""#, "true", "null"]
        );
        assert_eq!(nodes[&1].2, 3);
        assert_eq!(nodes[&2].2, 3);
        for id in [2, 6] {
            let name = nodes[&id].0.name.unwrap();
            assert_eq!(&bytes[name.start as usize..name.end as usize], br#""x""#);
        }
        assert_ne!(nodes[&2].0.ordinal, nodes[&6].0.ordinal);
    }

    #[test]
    fn malformed_or_over_limit_json_never_completes_a_structural_scan() {
        for bytes in [
            "{\"x\":1,}",
            "[1,]",
            "[01]",
            "[1e]",
            "\"\\uD800\"",
            "true false",
            "",
            "[\"unterminated",
        ] {
            assert!(
                scan(bytes.as_bytes(), 100, 8, |_| Ok(())).is_err(),
                "{bytes}"
            );
        }
        assert!(scan(b"[0,1,2]".as_slice(), 3, 8, |_| Ok(())).is_err());
        assert!(scan(b"[[[]]]".as_slice(), 100, 2, |_| Ok(())).is_err());
        assert!(scan(b"\"\xff\"".as_slice(), 100, 8, |_| Ok(())).is_err());
    }

    #[test]
    fn giant_scalars_and_names_are_streamed_without_value_sized_reads_or_events() {
        for scalar in [b'7', b'a'] {
            let quoted = scalar == b'a';
            let prefix: &[u8] = if quoted { b"{\"" } else { b"[" };
            let suffix: &[u8] = if quoted { b"\":\"long\"}" } else { b"]" };
            let source = Cursor::new(prefix)
                .chain(io::repeat(scalar).take(20 * 1024 * 1024))
                .chain(Cursor::new(suffix));
            let mut events = 0;
            assert_eq!(
                scan(source, 10, 8, |_| {
                    events += 1;
                    Ok(())
                })
                .unwrap(),
                2
            );
            assert_eq!(events, 4);
        }
    }

    #[test]
    fn a_cancelled_sink_stops_before_scanning_the_remaining_document() {
        let mut events = 0;
        let result = scan(b"[1,2,3]".as_slice(), 100, 8, |_| {
            events += 1;
            anyhow::bail!("cancelled")
        });
        assert!(result.is_err());
        assert_eq!(events, 1);
    }
}
