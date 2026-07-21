// Public contract pinned by this acceptance test:
//
//     pub fn validate_calendar(candidate: Calendar) -> Result<Calendar, InvalidCalendar>;
//
// The pure model-layer validation trims nonempty names and normalizes accepted
// six-digit hexadecimal colors to lowercase `#rrggbb` without changing the
// calendar's other fields.

use calendar::model::{Calendar, CalendarSource, validate_calendar};
use uuid::Uuid;

#[test]
fn phase10_calendar_validation() {
    let id = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let candidate = Calendar {
        id,
        name: "  Work  ".to_string(),
        color: "A1B2C3".to_string(),
        visible: false,
        read_only: true,
        source: CalendarSource::Local,
    };

    let normalized = validate_calendar(candidate).expect("a valid calendar must be accepted");
    assert_eq!(normalized.name, "Work", "name must be trimmed");
    assert_eq!(normalized.color, "#a1b2c3", "color must be canonicalized");
    assert_eq!(normalized.id, id, "id must be preserved");
    assert!(!normalized.visible, "visibility must be preserved");
    assert!(normalized.read_only, "read-only state must be preserved");
    assert!(matches!(normalized.source, CalendarSource::Local));

    assert_eq!(
        validate_calendar(Calendar {
            color: "#d4e5f6".to_string(),
            ..normalized.clone()
        })
        .expect("lowercase hex with a leading hash must be accepted")
        .color,
        "#d4e5f6"
    );
    assert!(
        validate_calendar(Calendar {
            name: " \t\n ".to_string(),
            ..normalized.clone()
        })
        .is_err(),
        "a whitespace-only name must be rejected"
    );
    assert!(
        validate_calendar(Calendar {
            color: "#12345".to_string(),
            ..normalized.clone()
        })
        .is_err(),
        "a color with the wrong digit count must be rejected"
    );
    assert!(
        validate_calendar(Calendar {
            color: "#12xz56".to_string(),
            ..normalized
        })
        .is_err(),
        "a color with non-hex characters must be rejected"
    );
}
