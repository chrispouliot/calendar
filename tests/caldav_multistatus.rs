// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub struct ResourceRecord {
//             pub href: String,
//             pub response_status: Option<u16>,
//             pub etag: Option<String>,
//             pub calendar_data: Option<String>,
//         }
//
//         pub fn parse_multistatus(xml: &str) -> Result<Vec<ResourceRecord>, ParseError>;
//     }

use calendar::backend::caldav::parse_multistatus;

#[test]
fn phase11_parses_caldav_multistatus_resources_and_rejects_malformed_xml() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:x="urn:unrelated">
  <d:response>
    <d:href>/calendars/ada/tea&amp;coffee.ics</d:href>
    <d:propstat>
      <d:prop>
        <x:getetag>unrelated-etag</x:getetag>
        <d:getetag>"etag-1"</d:getetag>
        <c:calendar-data><![CDATA[BEGIN:VCALENDAR
VERSION:2.0
END:VCALENDAR]]></c:calendar-data>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
    <d:propstat>
      <d:prop>
        <d:getetag>"must-not-leak"</d:getetag>
        <c:calendar-data>must-not-leak</c:calendar-data>
      </d:prop>
      <d:status>HTTP/1.1 404 Not Found</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/calendars/ada/deleted.ics</d:href>
    <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:response>
</d:multistatus>"#;

    let resources = parse_multistatus(xml).expect("well-formed multistatus must parse");
    assert_eq!(
        resources.len(),
        2,
        "both responses must be retained in document order"
    );

    assert_eq!(resources[0].href, "/calendars/ada/tea&coffee.ics");
    assert_eq!(resources[0].response_status, None);
    assert_eq!(resources[0].etag.as_deref(), Some("\"etag-1\""));
    assert_eq!(
        resources[0].calendar_data.as_deref(),
        Some("BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR")
    );

    assert_eq!(resources[1].href, "/calendars/ada/deleted.ics");
    assert_eq!(resources[1].response_status, Some(404));
    assert_eq!(resources[1].etag, None);
    assert_eq!(resources[1].calendar_data, None);

    assert!(
        parse_multistatus("<d:multistatus xmlns:d=\"DAV:\"><d:response>").is_err(),
        "truncated XML must return a parse error rather than partial resources"
    );
}
