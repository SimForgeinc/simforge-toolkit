use serde_json::json;

use super::*;

fn parse(line: &str) -> Result<Vec<Value>, CliError> {
    let tokens: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
    parse_operations(&tokens)
}

#[test]
fn flags_build_the_operation_json() {
    assert_eq!(
        parse("add-actor --id ego --catalog-id vehicle.sedan --lane-rsl 3:0:-1 --s 40.5 --speed-kph 30").unwrap(),
        [json!({"type": "addActor", "id": "ego", "catalogId": "vehicle.sedan", "laneRsl": "3:0:-1", "s": 40.5, "speedKph": 30.0})]
    );
    assert_eq!(
        parse(
            "add-actor --id ped --catalog-id pedestrian.adult --x -12.5 --z 4 --overlap-override"
        )
        .unwrap(),
        [
            json!({"type": "addActor", "id": "ped", "catalogId": "pedestrian.adult", "x": -12.5, "z": 4.0, "overlapOverride": true})
        ]
    );
    assert_eq!(
        parse("set-actor-static --id ped --static false").unwrap(),
        [json!({"type": "setActorStatic", "id": "ped", "static": false})]
    );
}

#[test]
fn repeated_points_become_the_route() {
    assert_eq!(
        parse("set-waypoint-route --id cross --actor-id ped --point -1,2 --point 3.5,-4 --start-s 2 --duration-s 6").unwrap(),
        [json!({"type": "setWaypointRoute", "id": "cross", "actorId": "ped",
                "points": [{"x": -1.0, "z": 2.0}, {"x": 3.5, "z": -4.0}], "startS": 2.0, "durationS": 6.0})]
    );
    let one =
        parse("set-waypoint-route --id c --actor-id p --point 1,2 --start-s 0 --duration-s 4")
            .unwrap_err();
    assert_eq!(one.code, "bad_value");
    assert_eq!(parse("set-waypoint-route --id c --actor-id p --point 1 --point 2,3 --start-s 0 --duration-s 4").unwrap_err().code, "bad_value");
}

#[test]
fn a_position_is_exactly_one_of_lane_or_point() {
    assert_eq!(
        parse("move-actor --id ego").unwrap_err().code,
        "missing_argument"
    );
    assert_eq!(
        parse("move-actor --id ego --lane-rsl 1:0:1 --s 3 --x 1 --z 2")
            .unwrap_err()
            .code,
        "conflicting_arguments"
    );
    assert_eq!(
        parse("move-actor --id ego --lane-rsl 1:0:1")
            .unwrap_err()
            .code,
        "missing_argument"
    );
    assert_eq!(
        parse("move-actor --id ego --x 1").unwrap_err().code,
        "missing_argument"
    );
}

#[test]
fn bounds_and_enums_are_refused_before_the_document() {
    assert_eq!(
        parse("set-actor-speed --id ego --speed-kph 161")
            .unwrap_err()
            .code,
        "bad_value"
    );
    assert_eq!(
        parse("set-clock --minutes 1440").unwrap_err().code,
        "bad_value"
    );
    assert_eq!(
        parse("set-clock --minutes 7.5").unwrap_err().code,
        "bad_value"
    );
    assert_eq!(
        parse("set-weather --weather sunny").unwrap_err().code,
        "bad_value"
    );
    assert_eq!(
        parse("add-lane-change --id e --actor-id a --direction up --start-s 1")
            .unwrap_err()
            .code,
        "bad_value"
    );
    assert_eq!(parse("set-clip").unwrap_err().code, "missing_argument");
    assert_eq!(
        parse("set-clip --warmup-seconds 2").unwrap(),
        [json!({"type": "setClip", "warmupSeconds": 2.0})]
    );
}

#[test]
fn operations_chain_into_one_transaction() {
    let ops = parse("set-weather --weather light_rain -- set-clock --minutes 1020 -- set-clip --clip-seconds 30").unwrap();
    assert_eq!(
        ops,
        [
            json!({"type": "setWeather", "weather": "light_rain"}),
            json!({"type": "setClock", "minutes": 1020}),
            json!({"type": "setClip", "clipSeconds": 30.0}),
        ]
    );
    assert_eq!(
        parse("set-weather --weather clear -- -- set-clock --minutes 1")
            .unwrap_err()
            .code,
        "missing_command"
    );
    assert_eq!(
        parse("frobnicate --x 1").unwrap_err().code,
        "unknown_command"
    );
}

#[test]
fn the_derived_subcommand_carries_chained_operations() {
    #[derive(Debug, clap::Parser)]
    struct Apply {
        file: String,
        #[command(subcommand)]
        operations: Operations,
    }
    use clap::Parser;
    let parsed = Apply::try_parse_from([
        "apply",
        "doc.json",
        "set-weather",
        "--weather",
        "fog_dense",
        "--",
        "set-ego",
        "--role-id",
        "ego",
    ])
    .unwrap();
    assert_eq!(
        parsed.operations.0,
        [
            json!({"type": "setWeather", "weather": "fog_dense"}),
            json!({"type": "setEgo", "roleId": "ego"})
        ]
    );
}
