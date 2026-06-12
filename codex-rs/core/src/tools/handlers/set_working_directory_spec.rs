use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub(crate) fn create_set_working_directory_tool() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: "set_working_directory".to_string(),
        description: "Change this session's working directory. Relative paths resolve from the current working directory. Later tool calls in the same response wait for this change and are cancelled if it fails.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            BTreeMap::from([(
                "path".to_string(),
                JsonSchema::string(Some("Existing directory path.".to_string())),
            )]),
            Some(vec!["path".to_string()]),
            /*additional_properties*/ Some(false.into()),
        ),
        output_schema: None,
    })
}
