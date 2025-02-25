use std::{
  fs::{metadata, File},
  io::{BufReader, Write},
  path::Path,
};

use byteorder::{BigEndian, WriteBytesExt};
use hex::FromHex;
use itertools::Itertools;
use md5::Digest;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
  decoders::RawDecodeParams,
  formats::bmff::{parse_file, FileBox},
  imgop::{raw::develop_raw_srgb, rescale_f32_to_u16, Dim2},
  formats::tiff::Rational,
  formats::tiff::SRational,
  Buffer, RawImageData, RawlerError, Result,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Md5Digest {
  digest: md5::Digest,
}

impl From<md5::Digest> for Md5Digest {
  fn from(digest: md5::Digest) -> Self {
    Self { digest }
  }
}

impl Serialize for Md5Digest {
  fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let s = format!("{:x}", self.digest);
    serializer.serialize_str(&s)
  }
}

impl<'de> Deserialize<'de> for Md5Digest {
  fn deserialize<D>(deserializer: D) -> std::result::Result<Md5Digest, D::Error>
  where
    D: Deserializer<'de>,
  {
    use serde::de::Error;
    let s = String::deserialize(deserializer)?;
    if s.len() != 32 {
      Err(D::Error::custom(format!("Invalid digest value: {}", s)))
    } else {
      Ok(Md5Digest {
        digest: Digest(<[u8; 16]>::from_hex(s).map_err(D::Error::custom)?),
      })
    }
  }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMetadata {
  file_size: u64,
  file_name: String,
  digest: Option<Md5Digest>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzerResult {
  pub file: FileMetadata,
  pub capture_info: CaptureInfo,
  pub raw_params: RawParams,
  pub format: Option<FormatDump>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureInfo {
  pub make: String,
  pub model: String,
  pub exposure_time: Option<Rational>,
  pub shutter_speed: Option<SRational>,
  pub exposure_bias: Option<SRational>,
  pub lens_make: Option<String>,
  pub lens_model: Option<String>,
  pub lens_spec: Option<[Rational; 4]>,
}
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawParams {
  pub raw_width: usize,
  pub raw_height: usize,
  pub bit_depth: usize,
  pub crops: [usize; 4],
  pub blacklevels: [u16; 4],
  pub whitelevels: [u16; 4],
  pub wb_coeffs: (Option<f32>, Option<f32>, Option<f32>, Option<f32>),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FormatDump {
  Cr3(FileBox),
  Cr2(Cr2Format),
}
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cr3Format {}
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cr2Format {}

pub fn analyze_file<P: AsRef<Path>>(path: P) -> Result<AnalyzerResult> {
  let fs_meta = metadata(&path).map_err(|e| RawlerError::with_io_error(&path, e))?;

  let mut raw_file = BufReader::new(File::open(&path).map_err(|e| RawlerError::with_io_error(&path, e))?);

  // Read whole raw file
  // TODO: Large input file bug, we need to test the raw file before open it
  let in_buffer = Buffer::new(&mut raw_file)?;

  // Get decoder or return
  let mut decoder = crate::get_decoder(&in_buffer)?;

  let digest = md5::compute(in_buffer.raw_buf());

  decoder.decode_metadata()?;

  let mut result = AnalyzerResult::default();
  result.file.file_name = path.as_ref().file_name().unwrap().to_string_lossy().to_string();
  result.file.file_size = fs_meta.len();
  result.file.digest = Some(digest.into());

  let rawimage = decoder.raw_image(RawDecodeParams::default(), true)?;
  result.capture_info.make = rawimage.make;
  result.capture_info.model = rawimage.model;
  result.raw_params.raw_width = rawimage.width;
  result.raw_params.raw_height = rawimage.height;
  result.raw_params.bit_depth = 16;
  result.raw_params.crops = rawimage.crops;
  result.raw_params.blacklevels = rawimage.blacklevels;
  result.raw_params.whitelevels = rawimage.whitelevels;
  result.raw_params.wb_coeffs = rawimage
    .wb_coeffs
    .iter()
    .map(|c| if c.is_nan() { None } else { Some(*c) })
    .collect_tuple()
    .unwrap();

  let mut in_f = File::open(&path).map_err(|e| RawlerError::with_io_error(&path, e))?;

  let filebox = parse_file(&mut in_f).unwrap();

  result.format = Some(FormatDump::Cr3(filebox));

  decoder.populate_capture_info(&mut result.capture_info)?;
  Ok(result)
}

pub fn extract_raw_pixels<P: AsRef<Path>>(path: P, params: RawDecodeParams) -> Result<(usize, usize, Vec<u16>)> {
  let mut raw_file = BufReader::new(File::open(&path).map_err(|e| RawlerError::with_io_error(&path, e))?);

  // Read whole raw file
  // TODO: Large input file bug, we need to test the raw file before open it
  let in_buffer = Buffer::new(&mut raw_file)?;

  // Get decoder or return
  let mut decoder = crate::get_decoder(&in_buffer)?;

  decoder.decode_metadata()?;

  let rawimage = decoder.raw_image(params, false)?;

  match rawimage.data {
    RawImageData::Integer(buf) => Ok((rawimage.width, rawimage.height, buf)),
    RawImageData::Float(_) => todo!(),
  }
}

pub fn raw_to_srgb<P: AsRef<Path>>(path: P, params: RawDecodeParams) -> Result<(Vec<u16>, Dim2)> {
  let mut raw_file = BufReader::new(File::open(&path).map_err(|e| RawlerError::with_io_error(&path, e))?);

  // Read whole raw file
  // TODO: Large input file bug, we need to test the raw file before open it
  let in_buffer = Buffer::new(&mut raw_file)?;

  // Get decoder or return
  let mut decoder = crate::get_decoder(&in_buffer)?;
  decoder.decode_metadata()?;
  let rawimage = decoder.raw_image(params, false)?;
  let params = rawimage.develop_params()?;
  let buf = match rawimage.data {
    RawImageData::Integer(buf) => buf,
    RawImageData::Float(_) => todo!(),
  };
  let (srgbf, dim) = develop_raw_srgb(&buf, &params)?;
  let output = rescale_f32_to_u16(&srgbf, 0, u16::MAX);
  Ok((output, dim))
}

/// Dump raw pixel data as PGM
pub fn raw_as_pgm(width: usize, height: usize, buf: &[u16], writer: &mut dyn Write) -> std::io::Result<()> {
  let header = format!("P5 {} {} {}\n", width, height, 65535);
  writer.write_all(header.as_bytes())?;
  for px in buf {
    writer.write_u16::<BigEndian>(*px)?;
  }
  Ok(())
}

/// Dump raw pixel data as PPM
pub fn raw_as_ppm16(width: usize, height: usize, buf: &[u16], writer: &mut dyn Write) -> std::io::Result<()> {
  let header = format!("P6 {} {} {}\n", width, height, 65535);
  writer.write_all(header.as_bytes())?;
  for px in buf {
    writer.write_u16::<BigEndian>(*px)?;
  }
  Ok(())
}
