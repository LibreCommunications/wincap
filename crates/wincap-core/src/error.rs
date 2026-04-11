use std::fmt;

#[derive(Debug)]
pub enum WincapError {
    HResult {
        component: &'static str,
        hr: i32,
        context: String,
    },
    General {
        component: &'static str,
        message: String,
    },
}

impl fmt::Display for WincapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WincapError::HResult {
                component,
                hr,
                context,
            } => write!(f, "{component}: HRESULT {hr:#010X} \u{2014} {context}"),
            WincapError::General { component, message } => {
                write!(f, "{component}: {message}")
            }
        }
    }
}

impl std::error::Error for WincapError {}

impl From<windows::core::Error> for WincapError {
    fn from(e: windows::core::Error) -> Self {
        WincapError::HResult {
            component: "windows",
            hr: e.code().0,
            context: e.message().to_string(),
        }
    }
}

pub type WincapResult<T> = Result<T, WincapError>;

/// Call a Windows function that returns a `windows::core::Result<T>`,
/// unwrapping on success or returning a `WincapError` on failure.
macro_rules! hr_call {
    ($component:expr, $expr:expr) => {{
        let result = $expr;
        match result {
            Ok(v) => v,
            Err(e) => {
                return Err($crate::error::WincapError::HResult {
                    component: $component,
                    hr: e.code().0,
                    context: stringify!($expr).to_string(),
                });
            }
        }
    }};
}

pub(crate) use hr_call;
