// Copyright © 2023 Tobias J. Prisching <tobias.prisching@icloud.com> and CONTRIBUTORS
// See https://github.com/TechnikTobi/little_exif#license for licensing details

use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;

use crc::Width;

use crate::endian::*;
use crate::general_file_io::*;
use crate::riff_chunk::RiffChunk;
use crate::riff_chunk::RiffChunkDescriptor;

pub(crate) const RIFF_SIGNATURE:       [u8; 4] = [0x52, 0x49, 0x46, 0x46];
pub(crate) const WEBP_SIGNATURE:       [u8; 4] = [0x57, 0x45, 0x42, 0x50];
pub(crate) const VP8X_HEADER:          &str    = "VP8X";
pub(crate) const EXIF_CHUNK_HEADER:    &str    = "EXIF";

/// A WebP file starts as follows
/// - The RIFF signature: ASCII characters "R", "I", "F", "F"  -> 4 bytes
/// - The file size starting at offset 8                       -> 4 bytes
/// - The WEBP signature: ASCII characters "W", "E", "B", "P"  -> 4 bytes
/// This function checks these 3 sections and their correctness after making
/// sure that the file actually exists and can be opened. 
/// Finally, the file struct is returned for further processing
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
get_next_chunk
(
	file: &mut File
)
-> Result<RiffChunk, std::io::Error>
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
	let mut chunk_length = from_u8_vec_macro!(u32, &chunk_start[4..8].to_vec(), &Endian::Little);

	// Account for the possible padding byte
	chunk_length += chunk_length % 2;

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
		return Ok(RiffChunk::new(
			parsed_chunk_name as String, 
			chunk_length      as usize,
			chunk_data_buffer as Vec<u8>
		));
	}
	else
	{
		return io_error!(Other, "Could not parse RIFF fourCC chunk name!");
	}
}



/// Gets a descriptor of the next RIFF chunk, starting at the current file
/// cursor position. Advances the cursor to the start of the next chunk
fn
get_next_chunk_descriptor
(
	file: &mut File
)
-> Result<RiffChunkDescriptor, std::io::Error>
{
	let next_chunk_result = get_next_chunk(file);

	if let Ok(next_chunk) = next_chunk_result
	{
		return Ok(next_chunk.descriptor());
	}
	else
	{
		return Err(next_chunk_result.err().unwrap());
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



/// Reads the raw EXIF data from the WebP file. Note that if the file contains
/// multiple such chunks, the first one is returned and the others get ignored.
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

			// Note that we have to seek another byte in case the chunk is of 
			// uneven size to account for the padding byte that must be included
			if chunk_size % 2 == 1
			{
				perform_file_action!(file.seek(SeekFrom::Current(1i64)));
			}
		}

		// Update for next loop iteration
		chunk_index += 1;
	}
}



fn
convert_to_extended_format
(
	file: &mut File
)
-> Result<(), std::io::Error>
{
	// Start by getting the first chunk of the WebP file
	perform_file_action!(file.seek(SeekFrom::Start(12)));
	let first_chunk_result = get_next_chunk(file);

	// Check that this get operation was successful
	if first_chunk_result.is_err()
	{
		return Err(first_chunk_result.err().unwrap());
	}

	let first_chunk = first_chunk_result.unwrap();

	// Find out what simple type of WebP file we are dealing with
	match first_chunk.descriptor().header().as_str()
	{
		"VP8" 
			=> println!("VP8!"),
		"VP8L"
			=> return convert_VP8L_to_VP8X(file),
		_ 
			=> return io_error!(Other, "Expected either 'VP8 ' or 'VP8L' chunk for conversion!")
	}
	
	// Ok(())
	
	io_error!(Other, "Converting still on ToDo List!")
}



#[allow(non_snake_case)]
fn
convert_VP8L_to_VP8X
(
	file: &mut File
)
-> Result<(), std::io::Error>
{
	// Seek to size information of the file
	perform_file_action!(file.seek(SeekFrom::Start(0u64
		+ 4u64 // "RIFF"
		+ 4u64 // file size
		+ 4u64 // "WEBP"
		+ 4u64 // "VP8L"
		+ 4u64 // VP8L chunk size information
		+ 1u64 // 0x2F - See: https://developers.google.com/speed/webp/docs/webp_lossless_bitstream_specification#3_riff_header
	)));

	// Get the next 4 bytes (although we only need the next 28 bits)
	let mut width_height_info_buffer = [0u8; 4];
	if file.read(&mut width_height_info_buffer).unwrap() != 4
	{
		return io_error!(Other, "Could not read start of VP8L chunk that has width/height info!");
	}

	let width_height_info = from_u8_vec_macro!(u32, &width_height_info_buffer.to_vec(), &Endian::Little);
	println!("{:#028b}", width_height_info);
	
	let mut width  = 0;
	let mut height = 0;

	for bit_index in 0..14
	{
		width  |= ((width_height_info >> (27 - bit_index)) & 0x01) << (13 - (bit_index % 14));
	}

	for bit_index in 14..28
	{
		height |= ((width_height_info >> (27 - bit_index)) & 0x01) << (13 - (bit_index % 14));
	}

	println!("width:  {}", width);
	println!("height: {}", height);

	todo!()
}



fn
set_exif_flag
(
	path:  &Path,
	exif_flag_value: bool
)
-> Result<(), std::io::Error>
{
	// Parse the WebP file - if this fails, we surely can't read any metadata
	let parsed_webp_result = parse_webp(path);
	if let Err(error) = parsed_webp_result
	{
		return Err(error);
	}

	// Open the file for further processing
	let mut file = check_signature(path).unwrap();

	// Next, check if this is an Extended File Format WebP file
	// In this case, the first Chunk SHOULD have the type "VP8X"
	// Otherwise we have to create the VP8X chunk!
	if let Some(first_chunk) = parsed_webp_result.as_ref().unwrap().first()
	{
		// Compare the chunk descriptor header and call chunk creator if required
		if first_chunk.header().to_lowercase() != VP8X_HEADER.to_lowercase()
		{
			convert_to_extended_format(&mut file)?;
		}
	}
	else
	{
		return io_error!(Other, "Could not read first chunk descriptor of WebP file!");
	}	

	// At this point we know that we have a VP8X chunk at the expected location
	// So, read in the flags and set the EXIF flag accoring to the given bool
	let mut flag_buffer = vec![0u8; 4usize];
	perform_file_action!(file.seek(SeekFrom::Start(12u64 + 4u64 + 4u64)));
	if file.read(&mut flag_buffer).unwrap() != 4
	{
		return io_error!(Other, "Could not read flags of VP8X chunk!");
	}

	// Mask the old flag by either or-ing with 1 at the EXIF flag position for
	// setting it to true, or and-ing with 1 everywhere but the EXIF flag pos
	// to set it to false
	flag_buffer[0] = if exif_flag_value
	{
		flag_buffer[0] | 0x08
	}
	else
	{
		flag_buffer[0] & 0b11110111
	};

	// Write flag buffer back to the file
	perform_file_action!(file.seek(SeekFrom::Start(12u64 + 4u64 + 4u64)));
	perform_file_action!(file.write_all(&flag_buffer));

	Ok(())
}



fn
clear_metadata
(
	path: &Path
)
-> Result<(), std::io::Error>
{
	// This needs to perform the following
	// Remove the EXIF chunk(s) (may contain more than one but only first is used when reading)
	// Compute the new size
	// Reset the flag in the VP8X header
	// Re-Write everything back to the file

	// Check the file signature, parse it, check that it has a VP8X chunk and
	// the EXIF flag is set there
	let exif_check_result = check_exif_in_file(path);
	if exif_check_result.is_err()
	{
		match exif_check_result.as_ref().err().unwrap().to_string().as_str()
		{
			"No EXIF chunk according to VP8X flags!"
				=> return Ok(()),
			"Expected first chunk of WebP file to be of type 'VP8X' but instead got VP8L!"
				=> return Ok(()),
			_
				=> return Err(exif_check_result.err().unwrap())
		}
	}

	let (mut file, parse_webp_result) = exif_check_result.unwrap();

	// Get the old size as starting point for computing the new value
	// NOTE from the documentation:
	// As the size of any chunk is even, the size given by the RIFF header is also even.
	perform_file_action!(file.seek(SeekFrom::Start(4u64)));
	let mut size_buffer = [0u8; 4];
	file.read(&mut size_buffer).unwrap();
	let mut new_size = from_u8_vec_macro!(u32, &size_buffer.to_vec(), &Endian::Little);

	// Skip the WEBP signature
	perform_file_action!(file.seek(SeekFrom::Current(4i64)));

	for parsed_chunk in parse_webp_result
	{
		// At the start of each iteration, the file cursor is at the start of
		// the fourCC section of a chunk

		// Compute how many bytes this chunk has
		let parsed_chunk_byte_count = 
			4u64                            // fourCC section of EXIF chunk
			+ 4u64                          // size information of EXIF chunk
			+ parsed_chunk.len() as u64     // actual size of EXIF chunk data
			+ parsed_chunk.len() as u64 % 2 // accounting for possible padding byte
		;

		// Not an EXIF chunk, seek to next one and continue
		if parsed_chunk.header().to_lowercase() != EXIF_CHUNK_HEADER.to_lowercase()
		{
			perform_file_action!(file.seek(SeekFrom::Current(parsed_chunk_byte_count as i64)));
			continue;
		}

		// Get the current size of the file in bytes
		let old_file_byte_count = file.metadata().unwrap().len();

		// Get a backup of the current cursor position
		let exif_chunk_start_cursor_position = SeekFrom::Start(file.seek(SeekFrom::Current(0)).unwrap());

		// Skip the EXIF chunk ...
		perform_file_action!(file.seek(SeekFrom::Current(parsed_chunk_byte_count as i64)));

		// ...and copy everything afterwards into a buffer...
		let mut buffer = Vec::new();
		perform_file_action!(file.read_to_end(&mut buffer));

		// ...and seek back to where the EXIF chunk is located...
		perform_file_action!(file.seek(exif_chunk_start_cursor_position));

		// ...and overwrite the EXIF chunk...
		perform_file_action!(file.write_all(&buffer));

		// ...and finally update the size of the file
		perform_file_action!(file.set_len(old_file_byte_count - parsed_chunk_byte_count));

		// Additionally, update the size information that gets written to the 
		// file header after this loop
		new_size -= parsed_chunk_byte_count as u32;
	}

	// Seek to the head of the file and update the file size information there
	perform_file_action!(file.seek(SeekFrom::Start(4)));
	perform_file_action!(file.write_all(
		&to_u8_vec_macro!(u32, &new_size, &Endian::Little)
	));

	// Set the flags in the VP8X chunk. First, read in the current flags
	perform_file_action!(set_exif_flag(path, false));

	return Ok(());
}



fn
encode_metadata_webp
(
	exif_vec: &Vec<u8>
)
-> Vec<u8>
{
	// vector storing the data that will be returned
	let mut webp_exif: Vec<u8> = Vec::new();

	// Compute the length of the exif data chunk 
	// This does NOT include the fourCC and size information of that chunk 
	// Also does NOT include the padding byte, i.e. this value may be odd!
	let length = exif_vec.len() as u32;

	// Start with the fourCC chunk head and the size information.
	// Then copy the previously encoded EXIF data 
	webp_exif.extend([0x45, 0x58, 0x49, 0x46]);
	webp_exif.extend(to_u8_vec_macro!(u32, &length, &Endian::Little));
	webp_exif.extend(exif_vec.iter());

	// Add the padding byte if required
	if length % 2 != 0
	{
		webp_exif.extend([0x00]);
	}

	return webp_exif;
}



/// Writes the given generally encoded metadata to the WebP image file at 
/// the specified path. 
/// Note that *all* previously stored EXIF metadata gets removed first before
/// writing the "new" metadata. 
pub(crate) fn
write_metadata
(
	path:                     &Path,
	general_encoded_metadata: &Vec<u8>
)
-> Result<(), std::io::Error>
{
	// Clear the metadata from the file and return if this results in an error
	clear_metadata(path)?;

	// Encode the general metadata format to WebP specifications
	let encoded_metadata = encode_metadata_webp(general_encoded_metadata);

	// Open the file...
	let mut file = check_signature(path)?;

	// ...and find a location where to put the EXIF chunk
	// This is done by requesting a chunk descriptor as long as we find a chunk
	// that is both known and should be located *before* the EXIF chunk
	let pre_exif_chunks = [
		"VP8X",
		"VP8",
		"VP8L",
		"ICCP",
		"ANIM"
	];

	loop
	{
		// Request a chunk descriptor. If this fails, this is fails, check the
		// error - depending on its type, either continue normally or return it
		let chunk_descriptor_result = get_next_chunk_descriptor(&mut file);

		if let Ok(chunk_descriptor) = chunk_descriptor_result
		{
			let mut chunk_type_found_in_pre_exif_chunks = false;

			// Check header of chunk descriptor against any of the known chunks
			// that should come before the EXIF chunk
			for pre_exif_chunk in &pre_exif_chunks
			{
				chunk_type_found_in_pre_exif_chunks |= pre_exif_chunk.to_lowercase() == chunk_descriptor.header().to_lowercase();
			}

			if !chunk_type_found_in_pre_exif_chunks
			{
				break;
			}
		}
		else
		{
			match chunk_descriptor_result.as_ref().err().unwrap().kind()
			{
				std::io::ErrorKind::UnexpectedEof
					=> break, // No further chunks, place EXIF chunk here
				_
					=> return Err(chunk_descriptor_result.err().unwrap())
			}
		}
	}

	// Next, read remaining file into a buffer...
	let current_file_cursor = SeekFrom::Start(file.seek(SeekFrom::Current(0)).unwrap());
	let mut read_buffer = Vec::new();
	perform_file_action!(file.read_to_end(&mut read_buffer));

	// ...and write the EXIF chunk at the previously found location...
	perform_file_action!(file.seek(current_file_cursor));
	perform_file_action!(file.write_all(&encoded_metadata));

	// ...and writing back the remaining file content
	perform_file_action!(file.write_all(&read_buffer));


	// Update the file size information, first by reading in the current value...
	perform_file_action!(file.seek(SeekFrom::Start(4)));
	let mut file_size_buffer = [0u8; 4];
	perform_file_action!(file.read(&mut file_size_buffer));
	let mut file_size = from_u8_vec_macro!(u32, &file_size_buffer.to_vec(), &Endian::Little);

	// ...adding the byte count of the EXIF chunk...
	// (Note: Due to  the WebP specific encoding function, this vector already
	// contains the EXIF header characters and size information, as well as the
	// possible padding byte. Therefore, simply taking the length of this
	// vector takes their byte count also into account and no further values
	// need to be added)
	file_size += encoded_metadata.len() as u32;

	// ...and writing back to file...
	perform_file_action!(file.seek(SeekFrom::Start(4)));
	perform_file_action!(file.write_all(&to_u8_vec_macro!(u32, &file_size, &Endian::Little)));

	// ...and finally, set the EXIF flag
	perform_file_action!(set_exif_flag(path, true));

	return Ok(());
}



#[cfg(test)]
mod tests 
{
	use std::fs::copy;
	use std::fs::remove_file;
	use std::path::Path;

	#[test]
	fn
	clear_metadata()
	-> Result<(), std::io::Error>
	{
		// Remove file from previous run and replace it with fresh copy
		if let Err(error) = remove_file("tests/read_sample_no_exif.webp")
		{
			println!("{}", error);
		}
		copy("tests/read_sample.webp", "tests/read_sample_no_exif.webp")?;

		// Clear the metadata
		crate::webp::clear_metadata(Path::new("tests/read_sample_no_exif.webp"))?;

		Ok(())
	}
}