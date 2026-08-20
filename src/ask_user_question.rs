use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(crate) const TOOL_NAME: &str = "ask_user_question";
pub(crate) const MAX_QUESTIONS: usize = 4;
pub(crate) const MIN_OPTIONS: usize = 2;
pub(crate) const MAX_OPTIONS: usize = 6;
pub(crate) const MAX_HEADER_CHARS: usize = 12;
pub(crate) const MAX_QUESTION_CHARS: usize = 1_000;
pub(crate) const MAX_OPTION_LABEL_CHARS: usize = 80;
pub(crate) const MAX_OPTION_DESCRIPTION_CHARS: usize = 500;
pub(crate) const MAX_PREVIEW_CHARS: usize = 4_000;
pub(crate) const MAX_FREE_TEXT_BYTES: usize = 3_000;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AskUserQuestionArgs {
    pub(crate) questions: Vec<AskUserQuestion>,
}

impl AskUserQuestionArgs {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=MAX_QUESTIONS).contains(&self.questions.len()) {
            return Err(anyhow!(
                "{TOOL_NAME}.questions must contain between 1 and {MAX_QUESTIONS} questions"
            ));
        }
        for (question_index, question) in self.questions.iter().enumerate() {
            question.validate(question_index)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AskUserQuestion {
    pub(crate) question: String,
    pub(crate) header: String,
    pub(crate) options: Vec<AskUserQuestionOption>,
    #[serde(default)]
    pub(crate) multi_select: bool,
}

impl AskUserQuestion {
    fn validate(&self, question_index: usize) -> Result<()> {
        let number = question_index + 1;
        validate_non_empty_bounded(
            &self.question,
            MAX_QUESTION_CHARS,
            &format!("{TOOL_NAME}.questions[{question_index}].question"),
        )?;
        validate_non_empty_bounded(
            &self.header,
            MAX_HEADER_CHARS,
            &format!("{TOOL_NAME}.questions[{question_index}].header"),
        )?;
        if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&self.options.len()) {
            return Err(anyhow!(
                "{TOOL_NAME} question {number} must contain between {MIN_OPTIONS} and {MAX_OPTIONS} options"
            ));
        }
        let mut labels = HashSet::with_capacity(self.options.len());
        for (option_index, option) in self.options.iter().enumerate() {
            option.validate(question_index, option_index, self.multi_select)?;
            if !labels.insert(option.label.trim()) {
                return Err(anyhow!(
                    "{TOOL_NAME} question {number} contains duplicate option label `{}`",
                    option.label.trim()
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AskUserQuestionOption {
    pub(crate) label: String,
    pub(crate) description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) preview: Option<String>,
    #[serde(default)]
    pub(crate) default_selected: bool,
}

impl AskUserQuestionOption {
    fn validate(
        &self,
        question_index: usize,
        option_index: usize,
        multi_select: bool,
    ) -> Result<()> {
        validate_non_empty_bounded(
            &self.label,
            MAX_OPTION_LABEL_CHARS,
            &format!("{TOOL_NAME}.questions[{question_index}].options[{option_index}].label"),
        )?;
        validate_non_empty_bounded(
            &self.description,
            MAX_OPTION_DESCRIPTION_CHARS,
            &format!("{TOOL_NAME}.questions[{question_index}].options[{option_index}].description"),
        )?;
        if let Some(preview) = &self.preview
            && preview.chars().count() > MAX_PREVIEW_CHARS
        {
            return Err(anyhow!(
                "{TOOL_NAME}.questions[{question_index}].options[{option_index}].preview must contain at most {MAX_PREVIEW_CHARS} characters"
            ));
        }
        if self.default_selected && !multi_select {
            return Err(anyhow!(
                "{TOOL_NAME}.questions[{question_index}].options[{option_index}].defaultSelected is only valid when multiSelect is true"
            ));
        }
        Ok(())
    }
}

fn validate_non_empty_bounded(value: &str, maximum: usize, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(anyhow!("{field} must not be empty"));
    }
    if value.chars().count() > maximum {
        return Err(anyhow!("{field} must contain at most {maximum} characters"));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(anyhow!("{field} contains unsupported control characters"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AskUserQuestionResponse {
    pub(crate) answers: Vec<AskUserQuestionAnswer>,
    pub(crate) cancelled: bool,
}

impl AskUserQuestionResponse {
    pub(crate) fn answered(answers: Vec<AskUserQuestionAnswer>) -> Self {
        Self {
            answers,
            cancelled: false,
        }
    }

    pub(crate) fn cancelled() -> Self {
        Self {
            answers: Vec::new(),
            cancelled: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AskUserQuestionAnswer {
    pub(crate) question: String,
    pub(crate) selected_options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) free_text: Option<String>,
}

#[derive(Clone)]
pub(crate) struct AskUserQuestionRequester {
    requests: UnboundedSender<AskUserQuestionRequest>,
}

impl AskUserQuestionRequester {
    pub(crate) async fn request(
        &self,
        call_id: String,
        arguments: AskUserQuestionArgs,
        cancellation: &CancellationToken,
    ) -> Result<AskUserQuestionResponse> {
        let (response, response_rx) = oneshot::channel();
        self.requests
            .send(AskUserQuestionRequest {
                call_id,
                arguments,
                response: Some(response),
            })
            .map_err(|_| {
                anyhow!("{TOOL_NAME} is unavailable because the interactive TUI stopped")
            })?;
        tokio::select! {
            _ = cancellation.cancelled() => Err(anyhow!("{TOOL_NAME} was interrupted")),
            response = response_rx => response.map_err(|_| {
                anyhow!("{TOOL_NAME} was cancelled before receiving a response")
            }),
        }
    }
}

pub(crate) struct AskUserQuestionRequest {
    call_id: String,
    arguments: AskUserQuestionArgs,
    response: Option<oneshot::Sender<AskUserQuestionResponse>>,
}

impl AskUserQuestionRequest {
    pub(crate) fn call_id(&self) -> &str {
        &self.call_id
    }

    pub(crate) fn arguments(&self) -> &AskUserQuestionArgs {
        &self.arguments
    }

    pub(crate) fn respond(mut self, response: AskUserQuestionResponse) -> bool {
        self.response
            .take()
            .is_some_and(|sender| sender.send(response).is_ok())
    }
}

pub(crate) fn channel() -> (
    AskUserQuestionRequester,
    UnboundedReceiver<AskUserQuestionRequest>,
) {
    let (requests, receiver) = unbounded_channel();
    (AskUserQuestionRequester { requests }, receiver)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(label: &str, default_selected: bool) -> AskUserQuestionOption {
        AskUserQuestionOption {
            label: label.to_string(),
            description: format!("Description for {label}"),
            preview: None,
            default_selected,
        }
    }

    fn arguments(multi_select: bool) -> AskUserQuestionArgs {
        AskUserQuestionArgs {
            questions: vec![AskUserQuestion {
                question: "Which path should be used?".to_string(),
                header: "Path".to_string(),
                options: vec![option("First", multi_select), option("Second", false)],
                multi_select,
            }],
        }
    }

    #[test]
    fn validates_question_and_option_bounds() {
        let mut empty = arguments(false);
        empty.questions.clear();
        assert!(empty.validate().is_err());

        let mut too_many = arguments(false);
        let question = too_many.questions[0].clone();
        too_many.questions = vec![question; MAX_QUESTIONS + 1];
        assert!(too_many.validate().is_err());

        let mut too_few_options = arguments(false);
        too_few_options.questions[0].options.truncate(1);
        assert!(too_few_options.validate().is_err());

        let mut too_many_options = arguments(false);
        too_many_options.questions[0].options = (0..=MAX_OPTIONS)
            .map(|index| option(&format!("Option {index}"), false))
            .collect();
        assert!(too_many_options.validate().is_err());
    }

    #[test]
    fn rejects_single_select_defaults_and_duplicate_labels() {
        let mut default_on_single = arguments(false);
        default_on_single.questions[0].options[0].default_selected = true;
        assert!(default_on_single.validate().is_err());

        let mut duplicate = arguments(true);
        duplicate.questions[0].options[1].label = "First".to_string();
        assert!(duplicate.validate().is_err());

        assert!(arguments(true).validate().is_ok());
    }

    #[tokio::test]
    async fn requester_waits_for_an_explicit_response() {
        let (requester, mut requests) = channel();
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(async move {
            requester
                .request("call-1".to_string(), arguments(true), &cancellation)
                .await
        });
        let request = requests.recv().await.expect("question request");
        assert!(!task.is_finished());
        assert!(request.respond(AskUserQuestionResponse::cancelled()));
        assert_eq!(
            task.await.unwrap().unwrap(),
            AskUserQuestionResponse::cancelled()
        );
    }
}
