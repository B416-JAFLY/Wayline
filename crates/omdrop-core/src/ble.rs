//! AirDrop wake advertisement sent while discovering nearby devices.

use windows::{
    Devices::Bluetooth::Advertisement::{
        BluetoothLEAdvertisement, BluetoothLEAdvertisementPublisher,
        BluetoothLEAdvertisementPublisherStatus, BluetoothLEManufacturerData,
    },
    Storage::Streams::DataWriter,
    Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
};

use crate::sender::SendError;

// RoUninitialize must run on the thread which initialized this apartment.
struct Apartment(std::marker::PhantomData<std::rc::Rc<()>>);

impl Apartment {
    fn enter() -> Result<Self, SendError> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|error| SendError::invalid(format!("initialize Windows Runtime: {error}")))?;
        Ok(Self(std::marker::PhantomData))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

/// Owns the Windows BLE publisher. It stops advertising when dropped.
pub struct AirDropAdvert {
    publisher: BluetoothLEAdvertisementPublisher,
    _apartment: Apartment,
}

impl AirDropAdvert {
    pub fn start(legacy_shape: bool) -> Result<Self, SendError> {
        let apartment = Apartment::enter()?;
        let payload = continuity_payload(legacy_shape);

        // Release temporary WinRT objects before the apartment can be dropped on
        // a later publisher startup error.
        let publisher = {
            let writer = DataWriter::new()
                .map_err(|error| SendError::invalid(format!("create BLE data writer: {error}")))?;
            writer.WriteBytes(&payload).map_err(|error| {
                SendError::invalid(format!("write AirDrop BLE payload: {error}"))
            })?;
            let buffer = writer.DetachBuffer().map_err(|error| {
                SendError::invalid(format!("finish AirDrop BLE payload: {error}"))
            })?;
            let manufacturer = BluetoothLEManufacturerData::Create(0x004c, &buffer)
                .map_err(|error| SendError::invalid(format!("create Apple BLE data: {error}")))?;
            let advertisement = BluetoothLEAdvertisement::new().map_err(|error| {
                SendError::invalid(format!("create BLE advertisement: {error}"))
            })?;
            advertisement
                .ManufacturerData()
                .and_then(|data| data.Append(&manufacturer))
                .map_err(|error| SendError::invalid(format!("add Apple BLE data: {error}")))?;
            BluetoothLEAdvertisementPublisher::Create(&advertisement)
                .map_err(|error| SendError::invalid(format!("create BLE publisher: {error}")))?
        };
        let advert = Self {
            publisher,
            _apartment: apartment,
        };
        advert
            .publisher
            .Start()
            .map_err(|error| SendError::invalid(format!("start AirDrop BLE advert: {error}")))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let status = advert.publisher.Status().map_err(|error| {
                SendError::invalid(format!("read BLE publisher status: {error}"))
            })?;
            if status == BluetoothLEAdvertisementPublisherStatus::Started {
                break;
            }
            if status == BluetoothLEAdvertisementPublisherStatus::Aborted
                || std::time::Instant::now() >= deadline
            {
                return Err(SendError::invalid(format!(
                    "AirDrop BLE advertisement did not start (status {status:?})"
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Ok(advert)
    }
}

fn continuity_payload(legacy_shape: bool) -> [u8; 20] {
    let body: [u8; 18] = if legacy_shape {
        [0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    } else {
        [0x40, 0, 0, 0, 0, 0, 0, 0, 0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    };
    let mut payload = [0u8; 20];
    payload[0] = 0x05;
    payload[1] = body.len() as u8;
    payload[2..].copy_from_slice(&body);
    payload
}

impl Drop for AirDropAdvert {
    fn drop(&mut self) {
        let _ = self.publisher.Stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuity_payload_matches_current_and_legacy_airdrop_shapes() {
        let current = continuity_payload(false);
        assert_eq!(&current[..2], &[0x05, 18]);
        assert_eq!(&current[2..10], &[0x40, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&current[10..], &[0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

        let legacy = continuity_payload(true);
        assert_eq!(&legacy[..2], &[0x05, 18]);
        assert_eq!(&legacy[2..10], &[0; 8]);
        assert_eq!(&legacy[10..], &[0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
}
