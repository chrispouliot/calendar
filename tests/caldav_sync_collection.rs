// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub struct SyncCollection {
//             pub sync_token: String,
//             pub changes: Vec<ResourceRecord>,
//         }
//
//         pub fn parse_sync_collection(xml: &str) -> Result<SyncCollection, ParseError>;
//     }

use calendar::backend::caldav::parse_sync_collection;

#[test]
fn phase11_parses_root_dav_sync_token_and_ordered_resource_changes() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<dav:multistatus xmlns:dav="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:noise="urn:unrelated">
  <noise:sync-token>not-the-sync-token</noise:sync-token>
  <dav:sync-token>urn:sync:next&amp;page=2</dav:sync-token>
  <dav:response>
    <dav:href>changed&amp;kept.ics</dav:href>
    <dav:propstat>
      <dav:prop>
        <noise:getetag>wrong-etag</noise:getetag>
        <dav:getetag>"etag-1"</dav:getetag>
        <cal:calendar-data><![CDATA[BEGIN:VCALENDAR
VERSION:2.0
END:VCALENDAR]]></cal:calendar-data>
      </dav:prop>
      <dav:status>HTTP/1.1 200 OK</dav:status>
    </dav:propstat>
    <dav:propstat>
      <dav:prop><dav:getetag>must-not-leak</dav:getetag><cal:calendar-data>must-not-leak</cal:calendar-data></dav:prop>
      <dav:status>HTTP/1.1 404 Not Found</dav:status>
    </dav:propstat>
  </dav:response>
  <dav:response>
    <dav:href>/deleted.ics</dav:href>
    <dav:status>HTTP/1.1 404 Not Found</dav:status>
  </dav:response>
</dav:multistatus>"#;

    let collection = parse_sync_collection(xml).expect("a complete sync response must parse");
    assert_eq!(collection.sync_token, "urn:sync:next&page=2");
    assert_eq!(collection.changes.len(), 2, "changes retain response order");

    assert_eq!(collection.changes[0].href, "changed&kept.ics");
    assert_eq!(collection.changes[0].response_status, None);
    assert_eq!(collection.changes[0].etag.as_deref(), Some("\"etag-1\""));
    assert_eq!(
        collection.changes[0].calendar_data.as_deref(),
        Some("BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR")
    );

    assert_eq!(collection.changes[1].href, "/deleted.ics");
    assert_eq!(collection.changes[1].response_status, Some(404));
    assert_eq!(collection.changes[1].etag, None);
    assert_eq!(collection.changes[1].calendar_data, None);

    assert!(
        parse_sync_collection("<d:multistatus xmlns:d=\"DAV:\"/>").is_err(),
        "a root DAV sync-token is required"
    );
    assert!(
        parse_sync_collection("<d:multistatus xmlns:d=\"DAV:\"><d:sync-token/></d:multistatus>")
            .is_err(),
        "an empty root DAV sync-token is unusable"
    );
    assert!(
        parse_sync_collection("<d:multistatus xmlns:d=\"DAV:\"><d:sync-token>one</d:sync-token><d:sync-token>two</d:sync-token></d:multistatus>")
            .is_err(),
        "exactly one root DAV sync-token is required"
    );
    assert!(
        parse_sync_collection(
            "<d:multistatus xmlns:d=\"DAV:\"><d:sync-token>partial</d:sync-token>"
        )
        .is_err(),
        "truncated XML must not return partial output"
    );
}
