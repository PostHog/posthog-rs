use crate::errors::Error;
use crate::types::{FeatureKey, ProjectId};
use crate::Client;
use serde::Deserialize;

/// API for feature flags as described [here](https://posthog.com/docs/api/feature-flags)
pub trait FeatureFlagsAPI {
    /// Request to
    /// [/api/projects/{project_id}/feature_flags/](https://posthog.com/docs/api/feature-flags#get-api-projects-project_id-feature_flags)
    fn list_feature_flags(&self, project_id: ProjectId) -> Result<Vec<FeatureFlag>, Error>;

    /// Return a single feature flag based on the key
    fn get_feature_flag(
        &self,
        project_id: ProjectId,
        feature_flag_key: FeatureKey,
    ) -> Result<FeatureFlag, Error>;
}

impl FeatureFlagsAPI for Client {
    fn list_feature_flags(&self, project_id: ProjectId) -> Result<Vec<FeatureFlag>, Error> {
        let url = format!("/api/projects/{project_id}/feature_flags/");

        let response = self.get_request(url)?;
        let response: FeatureFlagResponse = response
            .json::<FeatureFlagResponse>()
            .map_err(|e| Error::Serialization(e.to_string()))?;
        let mut result = response.results;
        while response.next.is_some() {
            if let Some(next_url) = response.next.clone() {
                let response = self.get_request(next_url)?;
                let mut response: FeatureFlagResponse = response
                    .json::<FeatureFlagResponse>()
                    .map_err(|e| Error::Serialization(e.to_string()))?;
                result.append(&mut response.results);
            } else {
                break;
            }
        }
        Ok(result)
    }

    fn get_feature_flag(
        &self,
        project_id: ProjectId,
        feature_flag_key: FeatureKey,
    ) -> Result<FeatureFlag, Error> {
        let feature_flags = self.list_feature_flags(project_id)?;
        feature_flags
            .into_iter()
            .find(|feature_flag| feature_flag.key == feature_flag_key)
            .ok_or(Error::EmptyReply(format!(
                "Failed to find feature flag with key {feature_flag_key}"
            )))
    }
}

#[derive(Deserialize, Debug)]
pub struct FeatureFlagResponse {
    pub count: Option<u32>,
    pub next: Option<String>,
    pub previous: Option<String>,
    pub results: Vec<FeatureFlag>,
}

#[derive(Deserialize, Debug)]
pub struct FeatureFlag {
    pub id: i32,
    pub name: String,
    pub key: String,
    pub filters: FeatureFilters,
    pub deleted: bool,
    pub active: bool,
    pub created_by: FeatureFlagUser,
    pub created_at: String,
    pub is_simple_flag: bool,
    pub rollout_percentage: Option<i32>,
    pub ensure_experience_continuity: bool,
    pub experiment_set: Vec<serde_json::Value>,
    pub rollback_conditions: serde_json::Value,
    pub performed_rollback: Option<bool>,
    pub can_edit: Option<bool>,
    pub tags: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize, Debug)]
pub struct FeatureFlagUser {
    pub id: usize,
    pub uuid: String,
    pub distinct_id: String,
    pub first_name: String,
    pub email: String,
}

#[derive(Deserialize, Debug)]
pub struct FeatureFilters {
    pub groups: Vec<FeatureFilter>,
    pub multivariate: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct FeatureFilter {
    pub properties: Vec<FeatureFilterProperty>,
    pub rollout_percentage: Option<i32>,
}

#[derive(Deserialize, Debug)]
pub struct FeatureFilterProperty {
    pub key: String,
    pub operator: Option<String>,
    #[serde(alias = "type")]
    pub prop_type: String,
    pub value: serde_json::Value,
}
