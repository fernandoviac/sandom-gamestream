//! Reading H.264 Annex B byte streams: NAL units after start codes.

pub const NAL_SEQUENCE_PARAMETER_SET: u8 = 7;
pub const NAL_PICTURE_PARAMETER_SET: u8 = 8;
pub const NAL_ACCESS_UNIT_DELIMITER: u8 = 9;

/// The NAL units of an Annex B byte stream, without their start codes.
pub fn nal_units(stream: &[u8]) -> Vec<&[u8]> {
	let mut starts = Vec::new();
	let mut index = 0;
	while index + 3 <= stream.len() {
		if stream[index] == 0 && stream[index + 1] == 0 && stream[index + 2] == 1 {
			starts.push(index + 3);
			index += 3;
		} else {
			index += 1;
		}
	}
	let mut units = Vec::with_capacity(starts.len());
	for (position, &start) in starts.iter().enumerate() {
		let mut end = starts.get(position + 1).map_or(stream.len(), |next| next - 3);
		// A four-byte start code leaves one zero before the next unit.
		while end > start && stream[end - 1] == 0 && starts.get(position + 1).is_some() {
			end -= 1;
		}
		if end > start {
			units.push(&stream[start..end]);
		}
	}
	units
}

/// The access units of an Annex B stream whose units each start with an
/// access unit delimiter.
pub fn access_units(stream: &[u8]) -> Vec<&[u8]> {
	let mut starts = Vec::new();
	let mut index = 0;
	while index + 4 <= stream.len() {
		if stream[index] == 0
			&& stream[index + 1] == 0
			&& stream[index + 2] == 1
			&& stream[index + 3] & 0x1f == NAL_ACCESS_UNIT_DELIMITER
		{
			// Include a four-byte start code's leading zero.
			starts.push(if index > 0 && stream[index - 1] == 0 { index - 1 } else { index });
		}
		index += 1;
	}
	starts
		.iter()
		.enumerate()
		.map(|(position, &start)| &stream[start..*starts.get(position + 1).unwrap_or(&stream.len())])
		.collect()
}

/// Whether an access unit is a key frame: it carries the parameter sets.
pub fn is_key_frame(access_unit: &[u8]) -> bool {
	nal_units(access_unit).iter().any(|unit| unit[0] & 0x1f == NAL_SEQUENCE_PARAMETER_SET)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn nal_units_split_at_three_and_four_byte_start_codes() {
		let stream = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
		assert_eq!(nal_units(&stream), vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
	}

	#[test]
	fn access_units_start_at_their_delimiters() {
		let stream = [0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x41, 2];
		let units = access_units(&stream);
		assert_eq!(units, vec![&stream[..11], &stream[11..]]);
		assert!(is_key_frame(units[0]));
		assert!(!is_key_frame(units[1]));
	}
}
