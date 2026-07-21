// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub struct DiscoveredCalendar {
//             pub href: String,
//             pub display_name: Option<String>,
//             pub sync_token: Option<String>,
//             pub color: Option<String>,
//             pub writable: bool,
//         }
//
//         pub fn parse_current_user_principal(xml: &str) -> Result<String, ParseError>;
//         pub fn parse_calendar_home_set(xml: &str) -> Result<String, ParseError>;
//         pub fn parse_calendar_home_multistatus(
//             xml: &str,
//         ) -> Result<Vec<DiscoveredCalendar>, ParseError>;
//     }

use calendar::backend::caldav::{
    parse_calendar_home_multistatus, parse_calendar_home_set, parse_current_user_principal,
};

#[test]
fn phase11_discovers_calendars_from_successful_namespaced_properties() {
    let principal = parse_current_user_principal(
        r#"<x:multistatus xmlns:x="DAV:" xmlns:q="urn:unrelated">
             <x:response><x:href>/ignored</x:href>
               <x:propstat><x:prop><q:current-user-principal><q:href>/wrong</q:href></q:current-user-principal></x:prop><x:status>HTTP/1.1 200 OK</x:status></x:propstat>
               <x:propstat><x:prop><x:current-user-principal><x:href>/principals/ada&amp;bob/</x:href></x:current-user-principal></x:prop><x:status>HTTP/1.1 201 Created</x:status></x:propstat>
               <x:propstat><x:prop><x:current-user-principal><x:href>/must-not-leak</x:href></x:current-user-principal></x:prop><x:status>HTTP/1.1 404 Not Found</x:status></x:propstat>
             </x:response>
           </x:multistatus>"#,
    )
    .expect("a successful principal property must be parsed");
    assert_eq!(principal, "/principals/ada&bob/");

    let home = parse_calendar_home_set(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:z="urn:unrelated">
             <d:response><d:href>/principals/ada/</d:href>
               <d:propstat><d:prop><z:calendar-home-set><z:href>/wrong</z:href></z:calendar-home-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
               <d:propstat><d:prop><c:calendar-home-set><d:href>https://dav.example/calendars/ada&amp;friends/</d:href></c:calendar-home-set></d:prop><d:status>HTTP/1.1 204 No Content</d:status></d:propstat>
               <d:propstat><d:prop><c:calendar-home-set><d:href>/must-not-leak</d:href></c:calendar-home-set></d:prop><d:status>HTTP/1.1 403 Forbidden</d:status></d:propstat>
             </d:response>
           </d:multistatus>"#,
    )
    .expect("a successful calendar-home-set property must be parsed");
    assert_eq!(home, "https://dav.example/calendars/ada&friends/");

    let calendars = parse_calendar_home_multistatus(
        r#"<p:multistatus xmlns:p="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:apple="http://apple.com/ns/ical/" xmlns:noise="urn:unrelated">
             <p:response><p:href>https://dav.example/calendars/ada/</p:href>
               <p:propstat><p:prop>
                 <p:resourcetype><p:collection/><cal:calendar/><noise:calendar/></p:resourcetype>
                 <p:displayname>Home &amp; Work</p:displayname><p:sync-token>token-home</p:sync-token><apple:calendar-color>#336699FF</apple:calendar-color>
                 <p:current-user-privilege-set><p:privilege><p:write/></p:privilege></p:current-user-privilege-set>
               </p:prop><p:status>HTTP/1.1 200 OK</p:status></p:propstat>
             </p:response>
             <p:response><p:href>team&amp;friends/</p:href>
               <p:propstat><p:prop>
                 <p:resourcetype><cal:calendar/></p:resourcetype><noise:displayname>Wrong name</noise:displayname><p:displayname>Team &amp; Friends</p:displayname>
                 <p:current-user-privilege-set><p:privilege><p:read/></p:privilege></p:current-user-privilege-set>
               </p:prop><p:status>HTTP/1.1 200 OK</p:status></p:propstat>
             </p:response>
             <p:response><p:href>/calendars/ada/not-a-calendar/</p:href>
               <p:propstat><p:prop><p:resourcetype><p:collection/><noise:calendar/></p:resourcetype></p:prop><p:status>HTTP/1.1 200 OK</p:status></p:propstat>
             </p:response>
             <p:response><p:href>/calendars/ada/failed/</p:href>
               <p:propstat><p:prop><p:resourcetype><cal:calendar/></p:resourcetype><p:displayname>Must not leak</p:displayname></p:prop><p:status>HTTP/1.1 404 Not Found</p:status></p:propstat>
             </p:response>
           </p:multistatus>"#,
    )
    .expect("a calendar-home multistatus must parse");
    assert_eq!(
        calendars.len(),
        2,
        "the home collection and calendar children are retained in document order"
    );
    assert_eq!(calendars[0].href, "https://dav.example/calendars/ada/");
    assert_eq!(calendars[0].display_name.as_deref(), Some("Home & Work"));
    assert_eq!(calendars[0].sync_token.as_deref(), Some("token-home"));
    assert_eq!(calendars[0].color.as_deref(), Some("#336699FF"));
    assert!(calendars[0].writable);
    assert_eq!(calendars[1].href, "team&friends/");
    assert_eq!(calendars[1].display_name.as_deref(), Some("Team & Friends"));
    assert_eq!(calendars[1].sync_token, None);
    assert_eq!(calendars[1].color, None);
    assert!(
        !calendars[1].writable,
        "read-only calendars must not be assumed writable"
    );

    assert!(parse_current_user_principal("<d:multistatus xmlns:d=\"DAV:\"/>").is_err());
    assert!(parse_calendar_home_set("<d:multistatus xmlns:d=\"DAV:\"/>").is_err());
    assert!(
        parse_calendar_home_multistatus("<p:multistatus xmlns:p=\"DAV:\"><p:response>").is_err()
    );
}
