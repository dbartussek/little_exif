// Copyright © 2023 Tobias J. Prisching <tobias.prisching@icloud.com> and CONTRIBUTORS
// See https://github.com/TechnikTobi/little_exif#license for licensing details

use std::path::Path;
use std::io::Read;
use std::io::Write;
use std::io::Seek;
use std::io::SeekFrom;
use std::fs::File;
use std::fs::OpenOptions;

use crate::endian::*;
use crate::general_file_io::*;
use crate::riff_chunk::RiffChunkDescriptor;

pub(crate) const RIFF_SIGNATURE:       [u8; 4] = [0x52, 0x49, 0x46, 0x46];
pub(crate) const WEBP_SIGNATURE:       [u8; 4] = [0x57, 0x45, 0x42, 0x50];
pub(crate) const VP8X_HEADER:          &str    = "VP8X";
pub(crate) const EXIF_CHUNK_HEADER:    &str    = "EXIF";



fn
check_signature
(
	path: &Path
)
-> Result<File, std::io::Error>
{
	if !path.exists()
	{
		return io_error!(NotFound, "Can't open WebP file - File does not exist!");
	}

	let mut file = OpenOptions::new()
		.read(true)
		.write(true)
		.open(path)
		.expect("Could not open file");
	
	// Check the RIFF signature
	let mut riff_signature_buffer = [0u8; 4];
	perform_file_action!(file.read(&mut riff_signature_buffer));
	if !riff_signature_buffer.iter()
		.zip(RIFF_SIGNATURE.iter())
		.filter(|&(read, constant)| read == constant)
		.count() == RIFF_SIGNATURE.len()
	{
		return io_error!(
			InvalidData, 
			format!("Can't open WebP file - Expected RIFF signature but found {}!", from_u8_vec_macro!(String, &riff_signature_buffer.to_vec(), &Endian::Big))
		);
	}

	// Read the file size in byte and validate it using the file metadata
	let mut size_buffer = [0u8; 4];
	file.read(&mut size_buffer).unwrap();
	let byte_count = from_u8_vec_macro!(u32, &size_buffer.to_vec(), &Endian::Little);
	if file.metadata().unwrap().len() != (byte_count + 8) as u64
	{
		return io_error!(InvalidData, "Can't open WebP file - Promised byte count does not correspond with file size!");
	}

	// Check the WEBP signature
	let mut webp_signature_buffer = [0u8; 4];
	file.read(&mut webp_signature_buffer).unwrap();
	if !webp_signature_buffer.iter()
		.zip(WEBP_SIGNATURE.iter())
		.filter(|&(read, constant)| read == constant)
		.count() == WEBP_SIGNATURE.len()
	{
		return io_error!(
			InvalidData, 
			format!("Can't open WebP file - Expected WEBP signature but found {}!", from_u8_vec_macro!(String, &webp_signature_buffer.to_vec(), &Endian::Big))
		);
	}

	// Signature is valid - can proceed using the file as WebP file
	return Ok(file);
}



fn
get_next_chunk_descriptor
(
	file: &mut File
)
-> Result<RiffChunkDescriptor, std::io::Error>
{
	// Read the start of the chunk
	let mut chunk_start = [0u8; 8];
	let mut bytes_read = file.read(&mut chunk_start).unwrap();

	// Check that indeed 8 bytes were read
	if bytes_read != 8
	{
		return io_error!(UnexpectedEof, "Could not read start of chunk");
	}

	// Construct name of chunk and its length
	let chunk_name = String::from_utf8(chunk_start[0..4].to_vec());
	let chunk_length = from_u8_vec_macro!(u32, &chunk_start[4..8].to_vec(), &Endian::Little);

	// Read RIFF chunk data
	let mut chunk_data_buffer = vec![0u8; chunk_length as usize];
	bytes_read = file.read(&mut chunk_data_buffer).unwrap();
	if bytes_read != chunk_length as usize
	{
		return io_error!(
			Other, 
			format!("Could not read RIFF chunk data! Expected {chunk_length} bytes but read {bytes_read}")
		);
	}

	if let Ok(parsed_chunk_name) = chunk_name
	{
		return Ok(RiffChunkDescriptor::new(
			parsed_chunk_name, 
			chunk_length       as usize
		));
	}
	else
	{
		return io_error!(Other, "Could not parse RIFF fourCC chunk name!");
	}
}



/// "Parses" the WebP file by checking various properties:
/// - Can the file be opened and is the signature valid, including the file size?
/// - Are the chunks and their size descriptions OK? Relies on the local subroutine `get_next_chunk_descriptor`
pub(crate) fn
parse_webp
(
	path: &Path
)
-> Result<Vec<RiffChunkDescriptor>, std::io::Error>
{
	let file_result = check_signature(path);
	let mut chunks = Vec::new();

	if file_result.is_err()
	{
		return Err(file_result.err().unwrap());
	}

	let mut file = file_result.unwrap();

	// The amount of data we expect to read while parsing the chunks
	let expected_length = file.metadata().unwrap().len();

	// How much data we have parsed so far.
	// Starts with 12 bytes: 
	// - 4 bytes for RIFF signature
	// - 4 bytes for file size
	// - 4 bytes for WEBP signature
	// These bytes are already read in by the `check_signature` subroutine
	let mut parsed_length = 12u64;

	loop
	{
		let next_chunk_descriptor_result = get_next_chunk_descriptor(&mut file);
		if let Ok(chunk_descriptor) = next_chunk_descriptor_result
		{
			// The parsed length increases by the length of the chunk's 
			// header (4 byte) + it's size section (4 byte) and the payload
			// size, which is noted by the aforementioned size section
			parsed_length += 4u64 + 4u64 + chunk_descriptor.len() as u64;

			// Add the chunk descriptor
			chunks.push(chunk_descriptor);
			
			if parsed_length == expected_length
			{
				break;
			}			
		}
		else
		{
			// This is the case when the read of the next chunk descriptor 
			// fails due to not being able to fetch 8 bytes for the header and
			// chunk size information, indicating that there is no further data
			// in the file and we are done with parsing.
			// If the subroutine fails due to other reasons, the error gets
			// propagated further.
			if next_chunk_descriptor_result.as_ref().err().unwrap().kind() == std::io::ErrorKind::UnexpectedEof
			{
				break;
			}
			else
			{
				return Err(next_chunk_descriptor_result.err().unwrap());
			}
		}
	}

	return Ok(chunks);
}



fn
check_exif_in_file
(
	path: &Path
)
-> Result<(File, Vec<RiffChunkDescriptor>), std::io::Error>
{
	// Parse the WebP file - if this fails, we surely can't read any metadata
	let parsed_webp_result = parse_webp(path);
	if let Err(error) = parsed_webp_result
	{
		return Err(error);
	}

	// Next, check if this is an Extended File Format WebP file
	// In this case, the first Chunk SHOULD have the type "VP8X"
	// Otherwise, the file is either invalid ("VP8X" at wrong location) or a 
	// Simple File Format WebP file which don't contain any EXIF metadata.
	if let Some(first_chunk) = parsed_webp_result.as_ref().unwrap().first()
	{
		// Compare the chunk descriptor header.
		if first_chunk.header().to_lowercase() != VP8X_HEADER.to_lowercase()
		{
			return io_error!(
				Other, 
				format!("Expected first chunk of WebP file to be of type 'VP8X' but instead got {}!", first_chunk.header())
			);
		}
	}
	else
	{
		return io_error!(Other, "Could not read first chunk descriptor of WebP file!");
	}

	// Finally, check the flag by opening up the file and reading the data of
	// the VP8X chunk
	// Regarding the seek:
	// - RIFF + file size + WEBP -> 12 byte
	// - VP8X header             ->  4 byte
	// - VP8X chunk size         ->  4 byte
	let mut file = check_signature(path).unwrap();
	let mut flag_buffer = vec![0u8; 4usize];
	perform_file_action!(file.seek(SeekFrom::Start(12u64 + 4u64 + 4u64)));
	if file.read(&mut flag_buffer).unwrap() != 4
	{
		return io_error!(Other, "Could not read flags of VP8X chunk!");
	}

	// Check the 5th bit of the 32 bit flag_buffer. 
	// For further details see the Extended File Format section at
	// https://developers.google.com/speed/webp/docs/riff_container#extended_file_format
	if flag_buffer[0] & 0x08 != 0x08
	{
		return io_error!(Other, "No EXIF chunk according to VP8X flags!");
	}

	return Ok((file, parsed_webp_result.unwrap()));
}



pub(crate) fn
read_metadata
(
	path: &Path
)
-> Result<Vec<u8>, std::io::Error>
{
	// Check the file signature, parse it, check that it has a VP8X chunk and
	// the EXIF flag is set there
	let (mut file, parse_webp_result) = check_exif_in_file(path).unwrap();

	// At this point we have established that the file has to contain an EXIF
	// chunk at some point. So, now we need to find & return it
	// Start by seeking to the start of the first chunk and visiting chunk after
	// chunk via checking the type and seeking again to the next chunk via the
	// size information
	perform_file_action!(file.seek(SeekFrom::Start(12u64)));
	let mut header_buffer = vec![0u8; 4usize];
	let mut chunk_index = 0usize;
	loop
	{
		// Read the chunk type into the buffer
		if file.read(&mut header_buffer).unwrap() != 4
		{
			return io_error!(Other, "Could not read chunk type while traversing WebP file!");
		}
		let chunk_type = String::from_u8_vec(&header_buffer.to_vec(), &Endian::Little);

		// Check that this is still the type that we expect from the previous
		// parsing over the file
		// TODO: Maybe remove this part?
		let expected_chunk_type = parse_webp_result.iter().nth(chunk_index).unwrap().header();
		if chunk_type != expected_chunk_type
		{
			return io_error!(
				Other, 
				format!("Got unexpected chunk type! Exprected {} but got {}", expected_chunk_type, chunk_type)
			);
		}

		// Get the size of this chunk from the previous parsing process and skip
		// the 4 bytes regarding the size
		let chunk_size = parse_webp_result.iter().nth(chunk_index).unwrap().len();
		perform_file_action!(file.seek(SeekFrom::Current(4)));

		if chunk_type.to_lowercase() == EXIF_CHUNK_HEADER.to_lowercase()
		{
			// Read the EXIF chunk's data into a buffer
			let mut payload_buffer = vec![0u8; chunk_size];
			perform_file_action!(file.read(&mut payload_buffer));

			// Add the 6 bytes of the EXIF_HEADER as Prefix for the generic EXIF
			// data parser that is called on the result of this read function
			// Otherwise the result would directly start with the Endianness
			// information, leading to a failed EXIF header signature check in 
			// the function `decode_metadata_general`
			let mut raw_exif_data = EXIF_HEADER.to_vec();
			raw_exif_data.append(&mut payload_buffer);

			return Ok(raw_exif_data);
		}
		else
		{
			// Skip the entire chunk
			perform_file_action!(file.seek(SeekFrom::Current(chunk_size as i64)));
		}

		// Update for next loop iteration
		chunk_index += 1;
	}
}



pub(crate) fn
clear_metadata
(
	path: &Path
)
-> Result<u8, std::io::Error>
{
	// Check the file signature, parse it, check that it has a VP8X chunk and
	// the EXIF flag is set there
	let (mut file, parse_webp_result) = check_exif_in_file(path).unwrap();

	// This needs to perform the following
	// Remove the EXIF chunk(s) (may contain more than one but only first is used when reading)
	// Compute the new size
	// Reset the flag in the VP8X header
	// Re-Write everything back to the file

	return Ok(0);
}


/*
fn
encode_metadata_webp
(
	exif_vec: &Vec<u8>
)
-> Vec<u8>
{
	// vector storing the data that will be returned
	let mut webp_exif: Vec<u8> = Vec::new();

	// Compute the length of the exif data (includes the two bytes of the
	// actual length field)
	let length = 2u16 + (EXIF_HEADER.len() as u16) + (exif_vec.len() as u16);

	// Start with the APP1 marker and the length of the data
	// Then copy the previously encoded EXIF data 
	webp_exif.extend(to_u8_vec_macro!(u16, &JPG_APP1_MARKER, &Endian::Big));
	webp_exif.extend(to_u8_vec_macro!(u16, &length, &Endian::Big));
	webp_exif.extend(EXIF_HEADER.iter());
	webp_exif.extend(exif_vec.iter());

	return webp_exif;
}
*/



/// Writes the given generally encoded metadata to the WebP image file at 
/// the specified path. 
/// Note that any previously stored metadata under the APP1 marker gets removed
/// first before writing the "new" metadata. 
pub(crate) fn
write_metadata
(
	path: &Path,
	general_encoded_metadata: &Vec<u8>
)
-> Result<(), std::io::Error>
{
	return Ok(());
}
