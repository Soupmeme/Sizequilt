#![doc = include_str!("../readme.md")]
#![no_std]

/// Defines an external function to import. The library name is unused: the
/// app's build script links the import libraries.
#[macro_export]
macro_rules! link {
    ($library:literal $abi:literal $($link_name:literal)? fn $($function:tt)*) => (
        extern $abi {
            $(#[link_name=$link_name])?
            pub fn $($function)*;
        }
    )
}
