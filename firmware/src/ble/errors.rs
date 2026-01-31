use nrf_softdevice::ble::{self, central, gatt_client, gatt_server, peripheral};

#[derive(defmt::Format)]
pub enum BleError {
    Encryption(ble::EncryptError),
    ConnectError(central::ConnectError),
    DiscoveryError,
    WriteError(gatt_client::WriteError),
    ReadError(gatt_client::ReadError),
    AdvertiseError(peripheral::AdvertiseError),
    NotifyValueError(gatt_server::NotifyValueError),
    IndicateValueError(gatt_server::IndicateValueError),
    SetValueError(gatt_server::SetValueError),
}

impl From<central::ConnectError> for BleError {
    fn from(e: central::ConnectError) -> Self {
        return Self::ConnectError(e);
    }
}

impl From<gatt_client::DiscoverError> for BleError {
    fn from(_: gatt_client::DiscoverError) -> Self {
        return Self::DiscoveryError;
    }
}

impl From<gatt_client::WriteError> for BleError {
    fn from(e: gatt_client::WriteError) -> Self {
        return Self::WriteError(e);
    }
}

impl From<gatt_client::ReadError> for BleError {
    fn from(e: gatt_client::ReadError) -> Self {
        return Self::ReadError(e);
    }
}

impl From<peripheral::AdvertiseError> for BleError {
    fn from(e: peripheral::AdvertiseError) -> Self {
        return Self::AdvertiseError(e);
    }
}

impl From<gatt_server::SetValueError> for BleError {
    fn from(value: gatt_server::SetValueError) -> Self {
        return Self::SetValueError(value);
    }
}

impl From<gatt_server::NotifyValueError> for BleError {
    fn from(value: gatt_server::NotifyValueError) -> Self {
        return Self::NotifyValueError(value);
    }
}

impl From<gatt_server::IndicateValueError> for BleError {
    fn from(value: gatt_server::IndicateValueError) -> Self {
        return Self::IndicateValueError(value);
    }
}
