macro_rules! concat_param {
    ($param1:literal $(, $param:literal)*) => {
        concat!($param1 $(, ",", $param)*)
    };
}
