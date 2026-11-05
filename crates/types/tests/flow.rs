use nowcast_types::{
    BrickName, FLOW_ID_MAX_LEN, FlowId, FlowIdError, QualifiedFlowId, QualifiedFlowIdError,
};

#[test]
fn flow_id_accepts_up_to_256_bytes() {
    let id = "a".repeat(FLOW_ID_MAX_LEN);
    assert_eq!(FlowId::new(id.clone()).unwrap().as_str(), id);
}

#[test]
fn flow_id_rejects_more_than_256_bytes() {
    assert_eq!(
        FlowId::new("a".repeat(FLOW_ID_MAX_LEN + 1)),
        Err(FlowIdError { len: 257 })
    );
}

#[test]
fn flow_id_limit_counts_bytes_not_chars() {
    // 'é' is 2 bytes in UTF-8: 129 of them are 258 bytes but 129 chars.
    let id = "é".repeat(129);
    assert_eq!(FlowId::new(id), Err(FlowIdError { len: 258 }));
}

#[test]
fn qualified_flow_id_prefixes_the_brick_name() {
    let flow = QualifiedFlowId::new(
        BrickName::new("forge").unwrap(),
        FlowId::new("github.com/org/repo#main").unwrap(),
    );
    assert_eq!(flow.to_string(), "forge/github.com/org/repo#main");
}

#[test]
fn qualified_flow_id_splits_at_the_first_slash() {
    let flow = QualifiedFlowId::parse("forge/github.com/org/repo#main").unwrap();
    assert_eq!(flow.brick().as_str(), "forge");
    assert_eq!(flow.flow().as_str(), "github.com/org/repo#main");
    assert_eq!(flow.to_string(), "forge/github.com/org/repo#main");
}

#[test]
fn qualified_flow_id_rejects_malformed_input() {
    assert_eq!(
        QualifiedFlowId::parse("forge"),
        Err(QualifiedFlowIdError::MissingSeparator)
    );
    assert!(matches!(
        QualifiedFlowId::parse("Forge/x"),
        Err(QualifiedFlowIdError::Brick(_))
    ));
    let too_long = format!("forge/{}", "a".repeat(FLOW_ID_MAX_LEN + 1));
    assert!(matches!(
        QualifiedFlowId::parse(&too_long),
        Err(QualifiedFlowIdError::Flow(_))
    ));
}
