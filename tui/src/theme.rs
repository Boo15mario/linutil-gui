use clap::ValueEnum;

// Add the Theme name here for a new theme
// This is more secure than the previous list
// We cannot index out of bounds, and we are giving
// names to our various themes, making it very clear
// This will make it easy to add new themes
#[derive(Clone, Debug, PartialEq, Default, ValueEnum, Copy)]
pub enum Theme {
    #[default]
    Default,
    Compatible,
}

impl Theme {
    pub const fn dir_icon(&self) -> &'static str {
        match self {
            Theme::Default => "[DIR]",
            Theme::Compatible => "[DIR]",
        }
    }

    pub const fn cmd_icon(&self) -> &'static str {
        match self {
            Theme::Default => "[CMD]",
            Theme::Compatible => "[CMD]",
        }
    }

    pub const fn tab_icon(&self) -> &'static str {
        match self {
            Theme::Default => ">",
            Theme::Compatible => ">",
        }
    }

}

impl Theme {
    pub fn next(&mut self) {
        let position = *self as usize;
        let types = Theme::value_variants();
        *self = types[(position + 1) % types.len()];
    }

    pub fn prev(&mut self) {
        let position = *self as usize;
        let types = Theme::value_variants();
        *self = types[(position + types.len() - 1) % types.len()];
    }
}
