use strum_macros::AsRefStr;

#[derive(Debug, PartialEq, AsRefStr)]
pub enum HostMethod {
    FinalResult,
    FetchData,
}

#[derive(Debug, PartialEq, AsRefStr)]
pub enum GuestMethod {
    GetName,
    GetDescription,
    GetParams,
    Run
}
