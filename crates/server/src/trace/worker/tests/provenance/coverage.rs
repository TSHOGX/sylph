use super::*;

fn coverage_claim(
    owner: &AgentRef,
    key: &str,
    capture: &Capture,
    dimension: CoverageDimension,
    state: CoverageState,
) -> Claim {
    let mut value = claim(owner, key, agent_object(owner, "Coverage"), &[capture]);
    value.fact = Fact::Coverage {
        coverage: Coverage {
            subject: AnyObjectRef::Agent(owner.clone()),
            source: capture.source_position.source.clone(),
            dimension,
            interval: CoverageInterval::Source {
                first: capture.source_position.clone(),
                through: capture.source_position.clone(),
            },
            state,
        },
    };
    value
}

fn assert_dimensions(body: Value) {
    let bootstrap: ReaderBootstrap = serde_json::from_value(body).unwrap();
    let dimensions: BTreeMap<_, _> = bootstrap
        .header
        .coverage
        .into_iter()
        .map(|coverage| (coverage.dimension, coverage.state))
        .collect();
    let expected = [
        CoverageDimension::Messages,
        CoverageDimension::Operations,
        CoverageDimension::ExecutionBoundaries,
        CoverageDimension::Timing,
        CoverageDimension::ChildInterior,
        CoverageDimension::PayloadRanges,
        CoverageDimension::Context,
        CoverageDimension::Measurements,
    ];
    assert_eq!(dimensions.keys().copied().collect::<Vec<_>>(), expected);
    assert_eq!(
        dimensions[&CoverageDimension::Messages],
        ReadAvailability::Available
    );
    assert_eq!(
        dimensions[&CoverageDimension::Operations],
        ReadAvailability::Available
    );
    for dimension in [
        CoverageDimension::Timing,
        CoverageDimension::ExecutionBoundaries,
        CoverageDimension::Context,
    ] {
        assert!(
            matches!(dimensions[&dimension], ReadAvailability::Partial { .. }),
            "{dimension:?}: complete messages cannot cover this gap"
        );
    }
    assert!(matches!(
        dimensions[&CoverageDimension::ChildInterior],
        ReadAvailability::Unavailable { .. }
    ));
    assert!(matches!(
        dimensions[&CoverageDimension::Measurements],
        ReadAvailability::Unavailable { .. }
    ));
    assert!(matches!(
        dimensions[&CoverageDimension::PayloadRanges],
        ReadAvailability::Pending { .. }
    ));
}

#[tokio::test]
async fn complete_native_messages_cannot_hide_other_dimensions_or_missing_live_intervals() {
    let mut fixture = ProvenanceFixture::new().await;
    let owner = agent("recipient", "coverage");
    let native = fixture
        .capture(
            &owner,
            "native",
            br#"{"messages":"complete","timing":"complete"}"#,
        )
        .await;
    let live = fixture
        .capture(
            &owner,
            "live",
            br#"{"timing":"gap","child":"unavailable","payload":"pending"}"#,
        )
        .await;
    let mut values = vec![claim(
        &owner,
        "agent",
        agent_object(&owner, "Coverage"),
        &[&native],
    )];
    for (key, capture, dimension, state) in [
        (
            "messages",
            &native,
            CoverageDimension::Messages,
            CoverageState::Complete,
        ),
        (
            "operations",
            &live,
            CoverageDimension::Operations,
            CoverageState::Following,
        ),
        (
            "boundaries",
            &live,
            CoverageDimension::ExecutionBoundaries,
            CoverageState::Gap {
                reason: "Missing native execution boundary".into(),
            },
        ),
        (
            "live-timing",
            &live,
            CoverageDimension::Timing,
            CoverageState::Gap {
                reason: "Missing live clock interval".into(),
            },
        ),
        (
            "child",
            &live,
            CoverageDimension::ChildInterior,
            CoverageState::Unavailable {
                reason: "Child source unavailable".into(),
            },
        ),
        (
            "payload",
            &live,
            CoverageDimension::PayloadRanges,
            CoverageState::Pending {
                reason: "Payload transfer incomplete".into(),
            },
        ),
        (
            "context",
            &live,
            CoverageDimension::Context,
            CoverageState::Gap {
                reason: "Exact model input not observed".into(),
            },
        ),
        (
            "measurement",
            &live,
            CoverageDimension::Measurements,
            CoverageState::Unavailable {
                reason: "No counter baseline".into(),
            },
        ),
    ] {
        values.push(coverage_claim(&owner, key, capture, dimension, state));
    }
    fixture.publish(values, &[&native, &live]).await;
    assert_dimensions(fixture.bootstrap(&owner).await);

    // A complete interval in a separate source must coexist with the live gap,
    // including when the delayed interpretation advances at the same evidence cut.
    let complete = coverage_claim(
        &owner,
        "native-timing",
        &native,
        CoverageDimension::Timing,
        CoverageState::Complete,
    );
    fixture.publish(vec![complete], &[&native]).await;
    assert_dimensions(fixture.bootstrap(&owner).await);
    let retained: Vec<_> = claims(&fixture.generation)
        .await
        .into_iter()
        .filter_map(|claim| match claim.fact {
            Fact::Coverage { coverage } if coverage.dimension == CoverageDimension::Timing => {
                Some(coverage)
            }
            _ => None,
        })
        .collect();
    assert_eq!(retained.len(), 2);
    assert!(retained
        .iter()
        .any(|coverage| coverage.source == live.source_position.source
            && coverage.interval
                == CoverageInterval::Source {
                    first: live.source_position.clone(),
                    through: live.source_position.clone()
                }
            && coverage.state
                == CoverageState::Gap {
                    reason: "Missing live clock interval".into()
                }));
    assert!(retained
        .iter()
        .any(|coverage| coverage.source == native.source_position.source
            && coverage.state == CoverageState::Complete));
}
