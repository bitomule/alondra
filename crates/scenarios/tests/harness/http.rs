use alondra_scenarios::http::parse;

#[test]
fn a_chunked_body_is_put_back_together() {
    let raw = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
    let response = parse(raw).unwrap();
    assert_eq!((response.status, response.body.as_str()), (200, "abcde"));
}
