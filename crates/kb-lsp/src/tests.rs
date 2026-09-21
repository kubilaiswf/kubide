use super::*;

struct NoLines;
impl Lines for NoLines {
    fn line(&self, _: &Path, _: usize) -> Option<String> {
        None
    }
}

struct OneLine(&'static str);
impl Lines for OneLine {
    fn line(&self, _: &Path, _: usize) -> Option<String> {
        Some(self.0.to_string())
    }
}

#[test]
fn a_message_split_across_reads_comes_out_whole() {
    let bytes = frame(&json!({ "jsonrpc": "2.0", "method": "x", "params": { "s": "çay" } }));
    let mut framer = Framer::new();
    // Cut inside the header, then inside the two-byte `ç`.
    let cut = bytes.len() - 6;
    assert!(framer.feed(&bytes[..9]).is_empty());
    assert!(framer.feed(&bytes[9..cut]).is_empty());
    let out = framer.feed(&bytes[cut..]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["params"]["s"], "çay");
}

#[test]
fn two_messages_in_one_read_are_two_messages() {
    let mut bytes = frame(&json!({ "id": 1 }));
    bytes.extend(frame(&json!({ "id": 2 })));
    let out = Framer::new().feed(&bytes);
    assert_eq!(out.iter().map(|m| m["id"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2]);
}

#[test]
fn the_length_counts_bytes_not_characters() {
    let bytes = frame(&json!("ğ"));
    assert!(String::from_utf8_lossy(&bytes).starts_with("Content-Length: 4\r\n\r\n"));
}

#[test]
fn columns_convert_through_the_wire_encoding() {
    // `𝒳` is one character, two UTF-16 units, four UTF-8 bytes.
    let line = "a𝒳b";
    assert_eq!(Encoding::Utf16.to_units(line, 2), 3);
    assert_eq!(Encoding::Utf16.to_chars(line, 3), 2);
    assert_eq!(Encoding::Utf8.to_units(line, 2), 5);
    assert_eq!(Encoding::Utf8.to_chars(line, 5), 2);
    assert_eq!(Encoding::Utf32.to_chars(line, 2), 2);
    // Past the end clamps to the end rather than inventing columns.
    assert_eq!(Encoding::Utf16.to_chars(line, 99), 3);
}

#[test]
fn paths_survive_the_trip_through_a_uri() {
    let unix = Path::new("/home/me/my project/çay.rs");
    assert_eq!(uri_of(unix), "file:///home/me/my%20project/%C3%A7ay.rs");
    assert_eq!(path_of(&uri_of(unix)).unwrap(), unix);
    assert_eq!(uri_of(Path::new(r"C:\src\main.rs")), "file:///C:/src/main.rs");
    assert_eq!(path_of("file:///C:/src/main.rs").unwrap(), PathBuf::from("C:/src/main.rs"));
    assert_eq!(path_of("untitled:1"), None);
}

#[test]
fn diagnostics_arrive_with_character_columns() {
    let message = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": "file:///p/a.rs",
            "diagnostics": [{
                "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 4 } },
                "severity": 2,
                "source": "rustc",
                "message": "unused variable: `b`",
            }],
        },
    });
    let (event, reply) = decode(&message, &mut HashMap::new(), Encoding::Utf16, &OneLine("a𝒳b"));
    assert_eq!(reply, None);
    let Some(Event::Diagnostics { path, items }) = event else { panic!("no diagnostics") };
    assert_eq!(path, PathBuf::from("/p/a.rs"));
    assert_eq!(items[0].start, Pos { line: 0, col: 2 });
    assert_eq!(items[0].severity, Severity::Warning);
    assert_eq!(items[0].source.as_deref(), Some("rustc"));
}

#[test]
fn a_server_request_is_answered_not_left_hanging() {
    let message = json!({
        "jsonrpc": "2.0", "id": 7, "method": "workspace/configuration",
        "params": { "items": [{}, {}] },
    });
    let (event, reply) = decode(&message, &mut HashMap::new(), Encoding::Utf16, &NoLines);
    assert_eq!(event, None);
    assert_eq!(reply, Some(json!({ "jsonrpc": "2.0", "id": 7, "result": [null, null] })));
}

#[test]
fn a_definition_is_the_first_place_in_any_of_its_shapes() {
    let at = Some((PathBuf::from("/p/b.rs"), Pos { line: 4, col: 2 }));
    let range = json!({ "start": { "line": 4, "character": 2 }, "end": { "line": 4, "character": 5 } });
    for result in [
        json!({ "uri": "file:///p/b.rs", "range": range }),
        json!([{ "uri": "file:///p/b.rs", "range": range }]),
        json!([{ "targetUri": "file:///p/b.rs", "targetRange": range, "targetSelectionRange": range }]),
    ] {
        let mut pending = HashMap::from([(3, Request::Definition)]);
        let message = json!({ "jsonrpc": "2.0", "id": 3, "result": result });
        let (event, _) = decode(&message, &mut pending, Encoding::Utf32, &NoLines);
        assert_eq!(event, Some(Event::Definition { id: 3, at: at.clone() }));
        assert!(pending.is_empty());
    }
    let mut pending = HashMap::from([(3, Request::Definition)]);
    let (event, _) =
        decode(&json!({ "id": 3, "result": null }), &mut pending, Encoding::Utf32, &NoLines);
    assert_eq!(event, Some(Event::Definition { id: 3, at: None }));
}

#[test]
fn an_answer_nobody_asked_for_is_ignored() {
    let (event, reply) =
        decode(&json!({ "id": 99, "result": {} }), &mut HashMap::new(), Encoding::Utf16, &NoLines);
    assert_eq!((event, reply), (None, None));
}

#[test]
fn a_refusal_carries_its_reason() {
    let mut pending = HashMap::from([(5, Request::Hover)]);
    let message = json!({ "id": 5, "error": { "code": -32801, "message": "content modified" } });
    let (event, _) = decode(&message, &mut pending, Encoding::Utf16, &NoLines);
    assert_eq!(event, Some(Event::Failed { id: 5, message: "content modified".into() }));
}

#[test]
fn hover_reads_as_plain_text() {
    let mut pending = HashMap::from([(1, Request::Hover)]);
    let message = json!({ "id": 1, "result": { "contents": {
        "kind": "markdown",
        "value": "```rust\nfn add(a: i32) -> i32\n```\n---\nAdds one.",
    } } });
    let (event, _) = decode(&message, &mut pending, Encoding::Utf16, &NoLines);
    assert_eq!(event, Some(Event::Hover { id: 1, text: Some("fn add(a: i32) -> i32\nAdds one.".into()) }));
}

#[test]
fn completions_sort_as_the_server_ranked_them_and_lose_their_tab_stops() {
    let mut pending = HashMap::from([(2, Request::Completion)]);
    let message = json!({ "id": 2, "result": { "isIncomplete": false, "items": [
        { "label": "zeta", "sortText": "b", "kind": 6 },
        { "label": "push(…)", "sortText": "a", "kind": 2, "detail": "fn(&mut self, T)",
          "insertTextFormat": 2, "textEdit": { "newText": "push(${1:value})$0" } },
    ] } });
    let (event, _) = decode(&message, &mut pending, Encoding::Utf16, &NoLines);
    let Some(Event::Completions { items, .. }) = event else { panic!("no completions") };
    assert_eq!(items[0].label, "push(…)");
    assert_eq!(items[0].insert, "push(value)");
    assert_eq!(items[0].kind, Some("method"));
    assert_eq!(items[1].insert, "zeta");
}

#[test]
fn formatting_edits_convert_against_the_document() {
    let result = json!([{
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } },
        "newText": "x\r\ny",
    }]);
    let edits = text_edits(&result, Path::new("/p/a.rs"), Encoding::Utf16, &OneLine("a𝒳b"));
    assert_eq!(edits, [TextEdit {
        start: Pos { line: 0, col: 0 },
        end: Pos { line: 0, col: 2 },
        text: "x\ny".into(),
    }]);
}
