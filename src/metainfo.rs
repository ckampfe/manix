use std::convert::{TryFrom, TryInto};

pub(crate) struct MetaInfo {
    pub announce: String,
    pub info: Info,
}

pub(crate) struct Info {
    pub name: String,
    pub piece_length: usize,
    pub pieces: Vec<[u8; 20]>,
    pub length: usize,
}

impl TryFrom<nom_bencode::Bencode> for MetaInfo {
    type Error = std::io::Error;

    fn try_from(value: nom_bencode::Bencode) -> Result<Self, Self::Error> {
        match value {
            nom_bencode::Bencode::Dictionary(d) => {
                let a = b"announce".to_vec();
                let announce = {
                    let announce_bencode = d.get(&a).ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "no announce key present",
                        )
                    })?;
                    if let nom_bencode::Bencode::String(s) = announce_bencode {
                        Ok(std::str::from_utf8(s).unwrap().to_string())
                    } else {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "no announce key present",
                        ))
                    }
                }?;

                let i = b"info".to_vec();
                let info = {
                    let info_bencode = d.get(&i).ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no info key present")
                    })?;

                    if let nom_bencode::Bencode::Dictionary(info) = info_bencode {
                        let name_key = b"name".to_vec();
                        let piece_length_key = b"piece length".to_vec();
                        let pieces_key = b"pieces".to_vec();
                        let length_key = b"length".to_vec();
                        // let path_key = b"path".to_vec();

                        // for key in info.keys() {
                        //     println!("{}", std::str::from_utf8(key).unwrap());
                        // }

                        let name = info.get(&name_key).unwrap().unwrap_string();
                        let piece_length =
                            info.get(&piece_length_key).unwrap().unwrap_integer() as usize;
                        let pieces: Vec<[u8; 20]> = info
                            .get(&pieces_key)
                            .unwrap()
                            .unwrap_bytes()
                            .chunks(20)
                            .map(|chunk| chunk.try_into().unwrap())
                            .collect();
                        let length = info.get(&length_key).unwrap().unwrap_integer() as usize;
                        // let path = info.get(&path_key).unwrap().unwrap_string();

                        Ok(Info {
                            name,
                            piece_length,
                            pieces,
                            length,
                        })
                    } else {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "info must be a dict",
                        ))
                    }
                }?;

                Ok(MetaInfo { announce, info })
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "structure of bencode input was not a valid .torrent file",
            )),
        }
    }
}
