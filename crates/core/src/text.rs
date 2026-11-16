//! The two languages user-facing text is written in, and the macros that pick one: `t!` for a
//! literal, `tf!` for a formatted string.
pub use alondra_foundation::Lang;

#[doc(hidden)]
#[macro_export]
macro_rules! __lang_literal {
    ($lang:expr, $en:literal, $es:literal) => {
        match $lang {
            $crate::text::Lang::En => $en,
            $crate::text::Lang::Es => $es,
        }
    };
}
pub use crate::__lang_literal as t;

#[doc(hidden)]
#[macro_export]
macro_rules! __lang_format { ($lang:expr, $en:literal, $es:literal $(, $name:ident = $value:expr)* $(,)?) => { match $lang { $crate::text::Lang::En => format!($en $(, $name = $value)*), $crate::text::Lang::Es => format!($es $(, $name = $value)*) } }; }
pub use crate::__lang_format as tf;
