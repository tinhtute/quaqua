#[test]
fn livecheck_deliberate_failure() {
    assert_eq!(1, 2, "livecheck: deliberate CI failure");
}
