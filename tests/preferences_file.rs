// Pure contract for the key=value preferences file. Saving one preference
// must never discard another, which is why the entry helpers are exposed.

use calendar::preferences::{
    parse_preference_entries, render_preference_entries, upsert_preference_entry,
};

#[test]
fn parsing_ignores_noise_and_keeps_first_value_of_a_repeated_key() {
    let entries = parse_preference_entries(
        "time-format=24-hour\n\nnot a pair\n =blank-key\ndefault-reminder-timed = 5m \ntime-format=12-hour\n",
    );
    assert_eq!(
        entries,
        vec![
            ("time-format".to_owned(), "24-hour".to_owned()),
            ("default-reminder-timed".to_owned(), "5m".to_owned()),
        ]
    );
}

#[test]
fn updating_one_entry_preserves_the_others_in_place() {
    let mut entries =
        parse_preference_entries("time-format=12-hour\ndefault-reminder-all-day=1d\n");
    upsert_preference_entry(&mut entries, "time-format", "system");
    upsert_preference_entry(&mut entries, "default-reminder-timed", "15m");
    assert_eq!(
        render_preference_entries(&entries),
        "time-format=system\ndefault-reminder-all-day=1d\ndefault-reminder-timed=15m\n"
    );
    assert_eq!(
        parse_preference_entries(&render_preference_entries(&entries)),
        entries,
        "rendered output must parse back to the same entries",
    );
}
