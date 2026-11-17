use anyhow::Result;

use crate::herdr;
use crate::route::Teammate;

const SOURCE: &str = "kotarella1110.claude-code-agent-teams";

pub(crate) fn show(pane_id: &str, teammate: &Teammate) -> Result<()> {
    herdr::call(&report_args(pane_id, teammate))?;
    Ok(())
}

fn report_args(pane_id: &str, teammate: &Teammate) -> Vec<String> {
    let mut args: Vec<String> = [
        "pane",
        "report-metadata",
        pane_id,
        "--source",
        SOURCE,
        "--agent",
        "claude",
        "--display-agent",
        &teammate.name,
        "--token",
        &format!("teammate={}", teammate.name),
    ]
    .map(str::to_owned)
    .into();
    for (token, value) in [("team", &teammate.team), ("model", &teammate.model)] {
        if let Some(value) = value {
            args.extend(["--token".to_owned(), format!("{token}={value}")]);
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_args_name_the_teammate_and_add_the_team_and_model_when_known() {
        let teammate =
            Teammate { name: "api".into(), team: Some("auth".into()), model: Some("haiku".into()) };

        assert_eq!(
            report_args("w1:p2", &teammate).join(" "),
            "pane report-metadata w1:p2 --source kotarella1110.claude-code-agent-teams --agent claude \
             --display-agent api --token teammate=api --token team=auth --token model=haiku"
        );
    }

    #[test]
    fn report_args_leave_out_unknown_tokens() {
        let teammate = Teammate { name: "api".into(), team: None, model: None };

        assert!(
            report_args("w1:p2", &teammate).join(" ").ends_with("--display-agent api --token teammate=api")
        );
    }
}
