use serde::Deserialize;

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
    pub filters: serde_json::Value,
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
