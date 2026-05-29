use codex_utils_template::Template;
use std::borrow::Cow;
use std::sync::LazyLock;

const REVIEW_EXIT_SUCCESS_TEMPLATE_TEXT: &str =
    include_str!("../templates/review/exit_success.xml");
const REVIEW_EXIT_INTERRUPTED_TEMPLATE_TEXT: &str =
    include_str!("../templates/review/exit_interrupted.xml");

static REVIEW_EXIT_SUCCESS_TEMPLATE: LazyLock<Template> = LazyLock::new(|| {
    let normalized = normalize_review_template_line_endings(REVIEW_EXIT_SUCCESS_TEMPLATE_TEXT);
    Template::parse(normalized.as_ref())
        .unwrap_or_else(|err| panic!("review exit success template must parse: {err}"))
});

pub fn render_review_exit_success(results: &str) -> String {
    REVIEW_EXIT_SUCCESS_TEMPLATE
        .render([("results", results)])
        .unwrap_or_else(|err| panic!("review exit success template must render: {err}"))
}

pub fn render_review_exit_interrupted() -> String {
    normalize_review_template_line_endings(REVIEW_EXIT_INTERRUPTED_TEMPLATE_TEXT).into_owned()
}

fn normalize_review_template_line_endings(template: &str) -> Cow<'_, str> {
    if template.contains('\r') {
        Cow::Owned(template.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(template)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn render_review_exit_success_replaces_results_placeholder() {
        assert_eq!(
            render_review_exit_success("Finding A\nFinding B"),
            "<user_action>\n  <context>User initiated a review task. Here's the full review output from reviewer model. User may select one or more comments to resolve.</context>\n  <action>review</action>\n  <results>\n  Finding A\nFinding B\n  </results>\n  </user_action>\n"
        );
    }

    #[test]
    fn normalize_review_template_line_endings_rewrites_crlf() {
        assert_eq!(
            normalize_review_template_line_endings("<user_action>\r\n  <results>\r\n  None.\r\n"),
            "<user_action>\n  <results>\n  None.\n"
        );
    }
}
