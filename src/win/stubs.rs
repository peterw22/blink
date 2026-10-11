//! POSIX functions blink calls that the Windows host layer doesn't
//! implement yet. Each aborts with its name (see `super::missing`).
//! Remove a name from this list when implementing it elsewhere.

macro_rules! missing {
    ($($name:ident)*) => {$(
        #[unsafe(no_mangle)]
        pub extern "C" fn $name() -> ! {
            super::missing(stringify!($name))
        }
    )*};
}

missing! {

}
