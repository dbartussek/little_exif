use std::path::Path;
use std::io::Read;
use std::io::Write;
use std::io::Seek;
use std::io::SeekFrom;
use std::fs::File;
use std::fs::OpenOptions;

use crc::{Crc, CRC_32_ISO_HDLC};
use deflate::deflate_bytes_zlib;

use crate::png_chunk::{PngChunkOrdering, PngChunk};

pub const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
pub const RAW_PROFILE_TYPE_EXIF: [u8; 23] = [
	0x52, 0x61, 0x77, 0x20,								// Raw
	0x70, 0x72, 0x6F, 0x66, 0x69, 0x6C, 0x65, 0x20,		// profile
	0x74, 0x79, 0x70, 0x65, 0x20,						// type
	0x65, 0x78, 0x69, 0x66, 0x00, 0x00					// exif NUL NUL
];

fn
check_signature
(
	path: &Path
)
-> Result<File, String>
{
	if !path.exists()
	{
		return Err("Can't parse PNG file - File does not exist!".to_string());
	}

	let mut file = OpenOptions::new()
		.read(true)
		.open(path)
		.expect("Could not open file");
	
	// Check the signature
	let mut signature_buffer = [0u8; 8];
	file.read(&mut signature_buffer).unwrap();
	let signature_is_valid = signature_buffer.iter()
		.zip(PNG_SIGNATURE.iter())
		.filter(|&(read, constant)| read == constant)
		.count() == 8;

	if !signature_is_valid
	{
		return Err("Can't parse PNG file - Wrong signature!".to_string());
	}

	// Signature is valid - can proceed using the file as PNG file
	return Ok(file);
}



// TODO: Check if this is also affected by endianness
fn
get_next_chunk_descriptor
(
	file: &mut File
)
-> Result<PngChunk, String>
{
	// Read the start of the chunk
	let mut chunk_start = [0u8; 8];
	let mut bytes_read = file.read(&mut chunk_start).unwrap();

	// Check that indeed 8 bytes were read
	if bytes_read != 8
	{
		return Err("Could not read start of chunk".to_string());
	}

	// Construct name of chunk and its length
	let chunk_name = String::from_utf8((&chunk_start[4..8]).to_vec());
	let mut chunk_length = 0u32;
	for byte in &chunk_start[0..4]
	{
		chunk_length = chunk_length * 256 + *byte as u32;
	}

	// Read chunk data ...
	let mut chunk_data_buffer = vec![0u8; chunk_length as usize];
	bytes_read = file.read(&mut chunk_data_buffer).unwrap();
	if bytes_read != chunk_length as usize
	{
		return Err("Could not read chunk data".to_string());
	}

	// ... and CRC values
	let mut chunk_crc_buffer = [0u8; 4];
	bytes_read = file.read(&mut chunk_crc_buffer).unwrap();
	if bytes_read != 4
	{
		return Err("Could not read chunk CRC".to_string());
	}

	// Compute CRC on chunk
	let mut crc_input = Vec::new();
	crc_input.extend(chunk_start[4..8].iter());
	crc_input.extend(chunk_data_buffer.iter());

	let crc_struct = Crc::<u32>::new(&CRC_32_ISO_HDLC);
	let checksum = crc_struct.checksum(&crc_input) as u32;

	for i in 0..4
	{
		if ((checksum >> (8 * (3-i))) as u8) != chunk_crc_buffer[i]
		{
			return Err("Checksum check failed while reading PNG!".to_string());
		}
	}

	// If validating the chunk using the CRC was successful, return its descriptor
	// Note: chunk_length does NOT include the +4 for the CRC area!
	PngChunk::from_string(
		&chunk_name.unwrap(),
		chunk_length
	)
}



pub fn
parse_png
(
	path: &Path
)
-> Result<Vec<PngChunk>, String>
{
	let mut file = check_signature(path);
	let mut chunks = Vec::new();

	if file.is_err()
	{
		return Err(file.err().unwrap());
	}

	loop
	{
		if let Ok(chunk_descriptor) = get_next_chunk_descriptor(file.as_mut().unwrap())
		{
			chunks.push(chunk_descriptor);

			if chunks.last().unwrap().as_string() == "IEND".to_string()
			{
				break;
			}
		}
		else
		{
			return Err("Could not read next chunk".to_string());
		}
	}

	return Ok(chunks);
}

// Clears existing metadata from a png file
// Gets called before writing any new metadata
pub fn
clear_metadata_from_png
(
	path: &Path
)
-> Result<(), String>
{
	if let Ok(chunks) = parse_png(path)
	{
		let mut file = check_signature(path).unwrap();
		let mut seek_counter = 0u64;

		for chunk in &chunks
		{
			if chunk.as_string() == String::from("zTXt")
			{
				// Get to the next chunk...
				file.seek(SeekFrom::Current(chunk.length() as i64 + 12));

				// Copy data from there onwards into a buffer
				let mut buffer = Vec::new();
				let bytes_read = file.read_to_end(&mut buffer).unwrap();

				// Go back to the chunk to be removed
				// And overwrite it using the data from the buffer
				file.seek(SeekFrom::Start(seek_counter));
				file.write_all(&buffer);
				file.seek(SeekFrom::Start(seek_counter));
			}
			else
			{
				seek_counter += (chunk.length() as u64 + 12);
				file.seek(SeekFrom::Current(chunk.length() as i64 + 12));
			}
		}

		return Ok(());
	}
	else
	{
		return Err("Could not clear metadata from PNG".to_string());
	}
}

pub fn
write_metadata_to_png
(
	path: &Path,
	encoded_metadata: &Vec<u8>
)
-> Result<(), String>
{

	// First clear the existing metadata
	// This also parses the PNG and checks its validity, so it is safe to
	// assume that is, in fact, a usable PNG file
	if let Err(_) = clear_metadata_from_png(path)
	{
		return Err("Could not safely write new metadata to PNG".to_string());
	}

	let mut IHDR_length = 0u32;
	if let Ok(chunks) = parse_png(path)
	{
		IHDR_length = chunks[0].length();
	}

	let mut file = OpenOptions::new()
		.write(true)
		.read(true)
		.open(path)
		.expect("Could not open file");

	let seek_start = 0u64			// Skip ...
	+ PNG_SIGNATURE.len()	as u64	// 	PNG Signature
	+ IHDR_length			as u64	//	IHDR data section
	+ 12					as u64;	//	rest of IHDR chunk (length, type, CRC)

	// Get to first chunk after IHDR, copy all the data starting from there
	file.seek(SeekFrom::Start(seek_start));
	let mut buffer = Vec::new();
	file.read_to_end(&mut buffer);
	file.seek(SeekFrom::Start(seek_start));

	// Build data of new chunk
	let mut zTXt_chunk_data: Vec<u8> = vec![0x7a, 0x54, 0x58, 0x74];
	zTXt_chunk_data.extend(RAW_PROFILE_TYPE_EXIF.iter());
	zTXt_chunk_data.extend(deflate_bytes_zlib(&encoded_metadata).iter());

	// Compute CRC and append it to the chunk data
	let crc_struct = Crc::<u32>::new(&CRC_32_ISO_HDLC);
	let checksum = crc_struct.checksum(&zTXt_chunk_data) as u32;
	for i in 0..4
	{
		zTXt_chunk_data.push( (checksum >> (8 * (3-i))) as u8);		
	}

	// Write new data to PNG file
	// Start with length of the new chunk (subtracting 8 for type and CRC)
	let chunk_data_len = zTXt_chunk_data.len() as u32 - 8;
	for i in 0..4
	{
		file.write( &[(chunk_data_len >> (8 * (3-i))) as u8] );
	}

	// Write data of new chunk and rest of PNG file
	file.write_all(&zTXt_chunk_data);
	file.write_all(&buffer);

	return Ok(());
}