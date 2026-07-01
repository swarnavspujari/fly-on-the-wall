//! Deliberately empty seams for features that are OUT OF SCOPE (§4 of the
//! product spec). These exist so a future hosted-sharing or integration
//! feature has a place to plug in, and so nobody wires such logic into the
//! domain model ad hoc. Do not implement anything here.

/// Hosted share links / public note URLs would implement this. No
/// implementation ships with Looma; notes never leave the machine.
pub trait SharingProvider: Send + Sync {
    fn id(&self) -> &'static str;
}

/// Third-party app integrations (CRM, Notion, Slack, …) would implement
/// this. No implementation ships with Looma.
pub trait Integration: Send + Sync {
    fn id(&self) -> &'static str;
}
