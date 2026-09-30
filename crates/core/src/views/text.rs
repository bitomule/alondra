use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    En,
    Es,
}
macro_rules! t {
    ($lang:expr, $en:literal, $es:literal) => {
        match $lang {
            $crate::views::Lang::En => $en,
            $crate::views::Lang::Es => $es,
        }
    };
}
pub(crate) use t;
macro_rules! tf { ($lang:expr, $en:literal, $es:literal $(, $name:ident = $value:expr)* $(,)?) => { match $lang { $crate::views::Lang::En => format!($en $(, $name = $value)*), $crate::views::Lang::Es => format!($es $(, $name = $value)*) } }; }
pub(crate) use tf;
