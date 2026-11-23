use super::NdjsonDecoder;

#[test]
fn lines_split_across_chunks_are_reassembled() {
    let mut decoder = NdjsonDecoder::default();
    assert!(decoder.push(b"{\"a\":").is_empty());
    assert_eq!(
        decoder.push(b"1}\n{\"b\":2}\n{\"c\""),
        vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec()]
    );
    assert_eq!(decoder.push(b":3}\r\n\n"), vec![b"{\"c\":3}".to_vec()]);
    assert_eq!(decoder.finish(), None);
}

#[test]
fn a_final_line_without_newline_is_returned_by_finish() {
    let mut decoder = NdjsonDecoder::default();
    assert!(decoder.push(b"{\"done\":true}").is_empty());
    assert_eq!(decoder.finish(), Some(b"{\"done\":true}".to_vec()));
}
