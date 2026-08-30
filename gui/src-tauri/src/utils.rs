use chacha20poly1305::{
    Key, XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use lifehash_lib::Version::Version2;
use lifehash_lib::lifehash;
use log::info;
// use petname::{Generator, Petnames};
use serde::Serialize;
use serde::de::DeserializeOwned;
use shared_types::RgbaImage;
use std::fs;
use std::fs::File;
use std::io::Write;
use tauri::Manager;

pub fn petname(_data: &[u8]) -> Result<String, String> {
    // use sha2::{Digest, Sha256};
    // use rand_chacha::ChaCha20Rng;
    // use rand_chacha::rand_core::SeedableRng;
    // let sha256: [u8; 32] = Sha256::digest(data).into();
    // Petnames::medium()
    //     .generate(&mut ChaCha20Rng::from_seed(sha256), 2, "-")
    //     .ok_or("Failed to generate petname".to_string())
    Err("Not Implemented".to_string())
}

fn hash_256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

pub fn encrypt(data: &[u8], password: &[u8], nonce: [u8; 24]) -> Result<Vec<u8>, String> {
    let encryption_key: [u8; 32] = hash_256(password);
    let key: Key = Key::from(encryption_key);
    let cipher = XChaCha20Poly1305::new(&key);
    let nonce: XNonce = XNonce::from(nonce);
    cipher.encrypt(&nonce, data).map_err(|e| e.to_string())
}

pub fn decrypt(data: &[u8], password: &[u8], nonce: [u8; 24]) -> Result<Vec<u8>, String> {
    let encryption_key: [u8; 32] = hash_256(password);
    let key: Key = Key::from(encryption_key);
    let cipher = XChaCha20Poly1305::new(&key);
    let nonce: XNonce = XNonce::from(nonce);
    cipher.decrypt(&nonce, data).map_err(|e| e.to_string())
}

pub fn load_config_file<T: DeserializeOwned>(
    app: tauri::AppHandle,
    file_name: &str,
) -> Result<Option<T>, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file_path = dir.join(file_name);
    info!("Loading {file_name} from {:?}", file_path);
    if !file_path.exists() {
        Ok(None)
    } else {
        serde_json::from_reader(File::open(file_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
            .map(Some)
    }
}

pub fn save_config_file<T: Serialize>(
    app: tauri::AppHandle,
    file_name: &str,
    data: &T,
) -> Result<(), String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file_path = dir.join(file_name);
    info!("Saving {file_name} to {:?}", file_path);
    let mut file = File::create(file_name).map_err(|e| e.to_string())?;
    file.write_all(
        serde_json::to_string_pretty(data)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())
}

pub fn lifehash(data: &'_ [u8]) -> Result<RgbaImage, String> {
    let image = lifehash::from_data(data, Version2, 1, true)
        .map(|v| v.0)
        .map_err(|e| e.to_string())?;
    let tauri_image = if image.channels < 4 {
        let pixel_count = image.width * image.height;
        let mut rgba_pixels = Vec::with_capacity(pixel_count * 4);
        match image.channels {
            1 => {
                for &gray_value in &image.pixels {
                    rgba_pixels.push(gray_value); // Red = Gray
                    rgba_pixels.push(gray_value); // Green = Gray
                    rgba_pixels.push(gray_value); // Blue = Gray
                    rgba_pixels.push(255); // Alpha = Full opacity
                }
            }
            2 => {
                for ga_chunk in image.pixels.as_chunks::<2>().0 {
                    let gray_value = ga_chunk[0];
                    let alpha_value = ga_chunk[1];
                    rgba_pixels.push(gray_value); // Red = Gray
                    rgba_pixels.push(gray_value); // Green = Gray
                    rgba_pixels.push(gray_value); // Blue = Gray
                    rgba_pixels.push(alpha_value); // Alpha = Original alpha
                }
            }
            3 => {
                for rgb_chunk in image.pixels.as_chunks::<3>().0 {
                    rgba_pixels.push(rgb_chunk[0]); // Red
                    rgba_pixels.push(rgb_chunk[1]); // Green
                    rgba_pixels.push(rgb_chunk[2]); // Blue
                    rgba_pixels.push(255); // Alpha = Full opacity
                }
            }
            _ => unreachable!(),
        }
        RgbaImage {
            data: rgba_pixels,
            width: image.width as u32,
            height: image.height as u32,
        }
    } else {
        RgbaImage {
            data: image.pixels,
            width: image.width as u32,
            height: image.height as u32,
        }
    };
    Ok(tauri_image)
}
