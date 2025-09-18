/// Feature Flags Test with Mock Server
/// 
/// This example includes a mock HTTP server to test feature flags without a real PostHog instance.
/// Run with: cargo run --example feature_flags_with_mock --all-features
/// 
/// Requires adding to Cargo.toml dev-dependencies:
/// mockito = "1.0"

use posthog_rs::{FeatureFlag, FeatureFlagFilters, FeatureFlagCondition, Property, FlagValue, MultivariateFilter, MultivariateVariant, match_feature_flag};
use std::collections::HashMap;
use serde_json::json;

fn main() {
    println!("🧪 PostHog Feature Flags - Local Testing with Mock Data");
    println!("{}", "=".repeat(60));
    
    // Create mock feature flags that would normally come from the API
    let flags = create_mock_flags();
    
    println!("\n📋 Available Feature Flags:");
    for flag in &flags {
        println!("  - {}: {}", flag.key, if flag.active { "active" } else { "inactive" });
    }
    
    // Test users
    let test_users = vec![
        ("user-001", create_user_props("US", "premium", 30, true)),
        ("user-002", create_user_props("UK", "basic", 25, false)),
        ("user-003", create_user_props("FR", "enterprise", 45, true)),
        ("user-004", create_user_props("US", "basic", 22, false)),
        ("user-005", create_user_props("US", "premium", 35, true)),
    ];
    
    println!("\n🔬 Testing Feature Flags Locally:\n");
    
    for (user_id, properties) in test_users {
        println!("User: {} | Properties: {:?}", user_id, properties);
        println!("{}", "-".repeat(50));
        
        for flag in &flags {
            let result = match_feature_flag(flag, user_id, &properties);
            
            match result {
                Ok(FlagValue::Boolean(true)) => {
                    println!("  ✅ {}: ENABLED", flag.key);
                }
                Ok(FlagValue::Boolean(false)) => {
                    println!("  ❌ {}: DISABLED", flag.key);
                }
                Ok(FlagValue::String(variant)) => {
                    println!("  🔀 {}: Variant '{}'", flag.key, variant);
                }
                Err(e) => {
                    println!("  ⚠️  {}: INCONCLUSIVE - {}", flag.key, e.message);
                }
            }
        }
        println!();
    }
    
    // Interactive testing
    println!("{}", "=".repeat(60));
    println!("\n📝 Interactive Testing");
    println!("You can now test specific scenarios:\n");
    
    // Test specific scenarios
    test_percentage_rollout();
    test_property_matching();
    test_multivariate_experiment();
    
    println!("\n✅ All tests completed!");
}

fn create_mock_flags() -> Vec<FeatureFlag> {
    vec![
        // Simple percentage rollout flag
        FeatureFlag {
            key: "simple-rollout".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![
                    FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(50.0),
                        variant: None,
                    }
                ],
                multivariate: None,
                payloads: HashMap::new(),
            },
        },
        
        // Property-based targeting
        FeatureFlag {
            key: "premium-feature".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![
                    FeatureFlagCondition {
                        properties: vec![
                            Property {
                                key: "plan".to_string(),
                                value: json!(["premium", "enterprise"]),
                                operator: "exact".to_string(),
                                property_type: None,
                            }
                        ],
                        rollout_percentage: Some(100.0),
                        variant: None,
                    }
                ],
                multivariate: None,
                payloads: HashMap::new(),
            },
        },
        
        // Geographic targeting
        FeatureFlag {
            key: "us-only-feature".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![
                    FeatureFlagCondition {
                        properties: vec![
                            Property {
                                key: "country".to_string(),
                                value: json!("US"),
                                operator: "exact".to_string(),
                                property_type: None,
                            }
                        ],
                        rollout_percentage: Some(100.0),
                        variant: None,
                    }
                ],
                multivariate: None,
                payloads: HashMap::new(),
            },
        },
        
        // Age-based targeting with comparison operator
        FeatureFlag {
            key: "adult-content".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![
                    FeatureFlagCondition {
                        properties: vec![
                            Property {
                                key: "age".to_string(),
                                value: json!(21),
                                operator: "gte".to_string(),
                                property_type: None,
                            }
                        ],
                        rollout_percentage: None,
                        variant: None,
                    }
                ],
                multivariate: None,
                payloads: HashMap::new(),
            },
        },
        
        // A/B test with variants
        FeatureFlag {
            key: "homepage-experiment".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![
                    FeatureFlagCondition {
                        properties: vec![],
                        rollout_percentage: Some(100.0),
                        variant: None,
                    }
                ],
                multivariate: Some(MultivariateFilter {
                    variants: vec![
                        MultivariateVariant {
                            key: "control".to_string(),
                            rollout_percentage: 33.0,
                        },
                        MultivariateVariant {
                            key: "variant-a".to_string(),
                            rollout_percentage: 33.0,
                        },
                        MultivariateVariant {
                            key: "variant-b".to_string(),
                            rollout_percentage: 34.0,
                        },
                    ],
                }),
                payloads: HashMap::new(),
            },
        },
        
        // Beta users flag
        FeatureFlag {
            key: "beta-features".to_string(),
            active: true,
            filters: FeatureFlagFilters {
                groups: vec![
                    FeatureFlagCondition {
                        properties: vec![
                            Property {
                                key: "beta_user".to_string(),
                                value: json!(true),
                                operator: "exact".to_string(),
                                property_type: None,
                            }
                        ],
                        rollout_percentage: Some(100.0),
                        variant: None,
                    }
                ],
                multivariate: None,
                payloads: HashMap::new(),
            },
        },
    ]
}

fn create_user_props(country: &str, plan: &str, age: i32, beta: bool) -> HashMap<String, serde_json::Value> {
    let mut props = HashMap::new();
    props.insert("country".to_string(), json!(country));
    props.insert("plan".to_string(), json!(plan));
    props.insert("age".to_string(), json!(age));
    props.insert("beta_user".to_string(), json!(beta));
    props
}

fn test_percentage_rollout() {
    println!("🎲 Testing Percentage Rollout (50%):");
    println!("Testing 10 users to see distribution...");
    
    let flag = FeatureFlag {
        key: "fifty-fifty".to_string(),
        active: true,
        filters: FeatureFlagFilters {
            groups: vec![
                FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(50.0),
                    variant: None,
                }
            ],
            multivariate: None,
            payloads: HashMap::new(),
        },
    };
    
    let mut enabled_count = 0;
    for i in 0..10 {
        let user_id = format!("test-user-{}", i);
        let result = match_feature_flag(&flag, &user_id, &HashMap::new());
        if let Ok(FlagValue::Boolean(true)) = result {
            enabled_count += 1;
            print!("✓");
        } else {
            print!("✗");
        }
    }
    println!("\nEnabled for {}/10 users", enabled_count);
    println!();
}

fn test_property_matching() {
    println!("🎯 Testing Property Matching:");
    
    let flag = FeatureFlag {
        key: "country-and-plan".to_string(),
        active: true,
        filters: FeatureFlagFilters {
            groups: vec![
                FeatureFlagCondition {
                    properties: vec![
                        Property {
                            key: "country".to_string(),
                            value: json!("US"),
                            operator: "exact".to_string(),
                            property_type: None,
                        },
                        Property {
                            key: "plan".to_string(),
                            value: json!(["premium", "enterprise"]),
                            operator: "exact".to_string(),
                            property_type: None,
                        },
                    ],
                    rollout_percentage: Some(100.0),
                    variant: None,
                }
            ],
            multivariate: None,
            payloads: HashMap::new(),
        },
    };
    
    let test_cases = vec![
        (create_user_props("US", "premium", 30, false), true, "US + premium"),
        (create_user_props("US", "basic", 30, false), false, "US + basic"),
        (create_user_props("UK", "premium", 30, false), false, "UK + premium"),
        (create_user_props("US", "enterprise", 30, false), true, "US + enterprise"),
    ];
    
    for (props, expected, description) in test_cases {
        let result = match_feature_flag(&flag, "test-user", &props);
        let matches = matches!(result, Ok(FlagValue::Boolean(true)));
        
        if matches == expected {
            println!("  ✅ {}: {} (as expected)", description, if matches { "ENABLED" } else { "DISABLED" });
        } else {
            println!("  ❌ {}: {} (expected {})", description, if matches { "ENABLED" } else { "DISABLED" }, if expected { "ENABLED" } else { "DISABLED" });
        }
    }
    println!();
}

fn test_multivariate_experiment() {
    println!("🔬 Testing Multivariate Experiment:");
    println!("Testing variant distribution across users...");
    
    let flag = FeatureFlag {
        key: "three-way-test".to_string(),
        active: true,
        filters: FeatureFlagFilters {
            groups: vec![
                FeatureFlagCondition {
                    properties: vec![],
                    rollout_percentage: Some(100.0),
                    variant: None,
                }
            ],
            multivariate: Some(MultivariateFilter {
                variants: vec![
                    MultivariateVariant {
                        key: "red".to_string(),
                        rollout_percentage: 33.0,
                    },
                    MultivariateVariant {
                        key: "green".to_string(),
                        rollout_percentage: 33.0,
                    },
                    MultivariateVariant {
                        key: "blue".to_string(),
                        rollout_percentage: 34.0,
                    },
                ],
            }),
            payloads: HashMap::new(),
        },
    };
    
    let mut variant_counts: HashMap<String, i32> = HashMap::new();
    
    for i in 0..30 {
        let user_id = format!("experiment-user-{}", i);
        let result = match_feature_flag(&flag, &user_id, &HashMap::new());
        
        if let Ok(FlagValue::String(variant)) = result {
            *variant_counts.entry(variant).or_insert(0) += 1;
        }
    }
    
    for (variant, count) in variant_counts {
        println!("  {} variant: {} users ({}%)", variant, count, (count as f64 / 30.0 * 100.0) as i32);
    }
    println!();
}