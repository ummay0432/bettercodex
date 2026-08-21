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
pub(crate) const MAX_QUESTION_CHARS: usize = 80;
pub(crate) const MAX_OPTION_LABEL_CHARS: usize = 24;
pub(crate) const MAX_OPTION_DESCRIPTION_CHARS: usize = 500;
pub(crate) const MAX_PREVIEW_CHARS: usize = 4_000;
pub(crate) const MAX_FREE_TEXT_BYTES: usize = 256;
pub(crate) const MAX_RESPONSE_JSON_BYTES: usize = 6 * 1024;

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
        if let Some(preview) = &self.preview {
            if preview.chars().count() > MAX_PREVIEW_CHARS {
                return Err(anyhow!(
                    "{TOOL_NAME}.questions[{question_index}].options[{option_index}].preview must contain at most {MAX_PREVIEW_CHARS} characters"
                ));
            }
            validate_supported_controls(
                preview,
                &format!("{TOOL_NAME}.questions[{question_index}].options[{option_index}].preview"),
            )?;
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
    validate_supported_controls(value, field)
}

fn validate_supported_controls(value: &str, field: &str) -> Result<()> {
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

    pub(crate) fn validate_for(&self, arguments: &AskUserQuestionArgs) -> Result<()> {
        if self.cancelled {
            if !self.answers.is_empty() {
                return Err(anyhow!("{TOOL_NAME} cancellation must not contain answers"));
            }
        } else {
            if self.answers.len() != arguments.questions.len() {
                return Err(anyhow!(
                    "{TOOL_NAME} response must contain one answer for every submitted question"
                ));
            }
            for (question_index, (answer, question)) in
                self.answers.iter().zip(&arguments.questions).enumerate()
            {
                if answer.question != question.question {
                    return Err(anyhow!(
                        "{TOOL_NAME}.answers[{question_index}].question does not match the submitted question"
                    ));
                }
                let mut selected = HashSet::with_capacity(answer.selected_options.len());
                for label in &answer.selected_options {
                    if !selected.insert(label.as_str())
                        || !question.options.iter().any(|option| option.label == *label)
                    {
                        return Err(anyhow!(
                            "{TOOL_NAME}.answers[{question_index}].selectedOptions contains an unknown or duplicate option"
                        ));
                    }
                }
                if !question.multi_select && answer.selected_options.len() > 1 {
                    return Err(anyhow!(
                        "{TOOL_NAME}.answers[{question_index}].selectedOptions contains multiple choices for a single-select question"
                    ));
                }
                if let Some(free_text) = &answer.free_text {
                    if free_text.trim().is_empty() {
                        return Err(anyhow!(
                            "{TOOL_NAME}.answers[{question_index}].freeText must not be empty"
                        ));
                    }
                    if free_text.len() > MAX_FREE_TEXT_BYTES {
                        return Err(anyhow!(
                            "{TOOL_NAME}.answers[{question_index}].freeText exceeds the {MAX_FREE_TEXT_BYTES}-byte limit"
                        ));
                    }
                    validate_supported_controls(
                        free_text,
                        &format!("{TOOL_NAME}.answers[{question_index}].freeText"),
                    )?;
                }
                let supplied_answers = answer
                    .selected_options
                    .len()
                    .saturating_add(usize::from(answer.free_text.is_some()));
                if supplied_answers == 0 || (!question.multi_select && supplied_answers > 1) {
                    return Err(anyhow!(
                        "{TOOL_NAME}.answers[{question_index}] must contain one valid decision"
                    ));
                }
            }
        }
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_RESPONSE_JSON_BYTES {
            return Err(anyhow!(
                "{TOOL_NAME} response exceeds the {MAX_RESPONSE_JSON_BYTES}-byte serialized limit"
            ));
        }
        Ok(())
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

    fn maximum_arguments() -> AskUserQuestionArgs {
        let question = "😀".repeat(MAX_QUESTION_CHARS);
        let option_suffixes = ['😀', '😁', '😂', '😃', '😄', '😅'];
        let options = option_suffixes
            .into_iter()
            .take(MAX_OPTIONS)
            .map(|suffix| AskUserQuestionOption {
                label: format!("{}{}", "😀".repeat(MAX_OPTION_LABEL_CHARS - 1), suffix),
                description: "Consequence".to_string(),
                preview: Some("line one\n\tline two".to_string()),
                default_selected: false,
            })
            .collect::<Vec<_>>();
        AskUserQuestionArgs {
            questions: (0..MAX_QUESTIONS)
                .map(|index| AskUserQuestion {
                    question: question.clone(),
                    header: format!("Q{}", index + 1),
                    options: options.clone(),
                    multi_select: true,
                })
                .collect(),
        }
    }

    #[test]
    fn maximum_unicode_response_fits_the_serialized_contract() {
        let arguments = maximum_arguments();
        arguments
            .validate()
            .unwrap_or_else(|error| panic!("maximum arguments should validate: {error}"));
        let answers = arguments
            .questions
            .iter()
            .map(|question| AskUserQuestionAnswer {
                question: question.question.clone(),
                selected_options: question
                    .options
                    .iter()
                    .map(|option| option.label.clone())
                    .collect(),
                free_text: Some("\"".repeat(MAX_FREE_TEXT_BYTES)),
            })
            .collect();
        let response = AskUserQuestionResponse::answered(answers);

        response
            .validate_for(&arguments)
            .unwrap_or_else(|error| panic!("maximum response should validate: {error}"));
        let encoded = serde_json::to_vec(&response)
            .unwrap_or_else(|error| panic!("maximum response should serialize: {error}"));
        assert!(encoded.len() <= MAX_RESPONSE_JSON_BYTES);
    }

    #[test]
    fn preview_accepts_newline_and_tab_but_rejects_other_controls() {
        let mut arguments = maximum_arguments();
        arguments.questions.truncate(1);
        arguments.questions[0].options.truncate(MIN_OPTIONS);
        arguments
            .validate()
            .unwrap_or_else(|error| panic!("newline and tab preview should validate: {error}"));

        arguments.questions[0].options[0].preview = Some("unsafe\u{0085}preview".to_string());
        let error = match arguments.validate() {
            Ok(()) => panic!("unsupported preview control should be rejected"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("preview contains unsupported control")
        );
    }

    #[test]
    fn response_rejects_oversized_or_unsupported_free_text() {
        let mut arguments = maximum_arguments();
        arguments.questions.truncate(1);
        arguments.questions[0].options.truncate(MIN_OPTIONS);
        let question = &arguments.questions[0];
        let mut response = AskUserQuestionResponse::answered(vec![AskUserQuestionAnswer {
            question: question.question.clone(),
            selected_options: Vec::new(),
            free_text: Some("a".repeat(MAX_FREE_TEXT_BYTES + 1)),
        }]);
        assert!(response.validate_for(&arguments).is_err());

        response.answers[0].free_text = Some("unsafe\u{0007}".to_string());
        assert!(response.validate_for(&arguments).is_err());
    }
}
