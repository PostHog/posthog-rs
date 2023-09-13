/// Result of a request to the API
pub type APIResult<T> = Result<T, crate::errors::Error>;
/// The id of a project given by PostHog
pub type ProjectId = &'static str;
/// The identifying key of a feature flag
pub type FeatureKey = &'static str;
