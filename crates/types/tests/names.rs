use nowcast_types::{BrickName, ClientKindName, NameError};

#[test]
fn accepts_lowercase_digits_and_hyphens() {
    for name in ["forge", "claude-code", "a", "0", "-", "quota-2"] {
        assert_eq!(BrickName::new(name).unwrap().as_str(), name);
        assert_eq!(ClientKindName::new(name).unwrap().as_str(), name);
    }
}

#[test]
fn rejects_empty() {
    assert_eq!(BrickName::new(""), Err(NameError::Empty));
    assert_eq!(ClientKindName::new(""), Err(NameError::Empty));
}

#[test]
fn rejects_anything_outside_the_alphabet() {
    for (name, index, found) in [
        ("Forge", 0, 'F'),
        ("for_ge", 3, '_'),
        ("forge/pr", 5, '/'),
        ("for ge", 3, ' '),
        ("forge.pr", 5, '.'),
        ("é", 0, 'é'),
        ("ok\n", 2, '\n'),
    ] {
        let expected = NameError::InvalidChar { index, found };
        assert_eq!(BrickName::new(name), Err(expected.clone()), "{name:?}");
        assert_eq!(ClientKindName::new(name), Err(expected), "{name:?}");
    }
}

#[test]
fn parses_through_from_str() {
    assert_eq!(
        "forge".parse::<BrickName>(),
        BrickName::new("forge"),
        "FromStr and new agree"
    );
    assert!("Forge".parse::<ClientKindName>().is_err());
}
