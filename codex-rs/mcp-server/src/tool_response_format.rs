use clap::ValueEnum;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ToolResponseFormat {
    #[default]
    Dual,
    StructuredOnly,
    ContentOnly,
}

impl ToolResponseFormat {
    pub(crate) fn includes_text_content(self) -> bool {
        !matches!(self, Self::StructuredOnly)
    }

    pub(crate) fn includes_structured_content(self) -> bool {
        !matches!(self, Self::ContentOnly)
    }
}
