#[cfg(feature = "async-client")]
use httpmock::prelude::*;
#[cfg(feature = "async-client")]
use posthog_rs::ClientOptionsBuilder;
#[cfg(feature = "async-client")]
use serde_json::json;
#[cfg(feature = "async-client")]
use std::collections::HashMap;
#[cfg(feature = "async-client")]
use std::time::Duration;

#[cfg(feature = "async-client")]
#[tokio::test]
async fn test_local_evaluation_with_mock_server() {
    let server = MockServer::start();

    // Mock the local evaluation endpoint
    let mock_flags = json!({
        "flags": [
            {
                "key": "feature-a",
                "active": true,
                "filters": {
                    "groups": [
                        {
                            "properties": [],
                            "rollout_percentage": 50.0,
                            "variant": null
                        }
                    ],
                    "multivariate": null,
                    "payloads": {}
                }
            },
            {
                "key": "feature-b",
                "active": true,
                "filters": {
                    "groups": [
                        {
                            "properties": [
                                {
                                    "key": "email",
                                    "value": "@company.com",
                                    "operator": "icontains"
                                }
                            ],
                            "rollout_percentage": 100.0,
                            "variant": null
                        }
                    ],
                    "multivariate": null,
                    "payloads": {}
                }
            }
        ],
        "group_type_mapping": {},
        "cohorts": {}
    });

    let eval_mock = server.mock(|when, then| {
        when.method(GET)
            .path("/api/feature_flag/local_evaluation/")
            .header("Authorization", "Bearer test_personal_key")
            .query_param("token", "test_project_key")
            .query_param("send_cohorts", "");
        then.status(200).json_body(mock_flags);
    });

    // Create client with local evaluation enabled
    let options = ClientOptionsBuilder::default()
        .host(server.base_url())
        .api_key("test_project_key".to_string())
        .personal_api_key("test_personal_key".to_string())
        .enable_local_evaluation(true)
        .poll_interval_seconds(60)
        .build()
        .unwrap();

    let client = posthog_rs::client(options).await;

    // Give it a moment to load initial flags
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Test local evaluation
    let mut properties = HashMap::new();
    properties.insert("email".to_string(), json!("test@company.com"));

    let result = client
        .get_feature_flag("feature-b", "user-123", None, Some(properties), None)
        .await;

    assert!(result.is_ok());
    // The actual result depends on whether the mock was hit and processed

    eval_mock.assert();
}
