// Command gen-fixtures emits the golden protobuf request/response bytes for
// Outh's Rust protocol tests.
//
// It constructs the exact same messages that gotohp's core/api.go constructs
// (same field values, copied construction code), marshals them with the real
// generated protobuf types from the gotohp clone, and prints one
// `name=hex` line per fixture to stdout:
//
//	go run . > ../../crates/outh-core/tests/fixtures/golden.txt
//
// The FIXED INPUTS below are shared verbatim with the Rust integration tests
// in crates/outh-core/tests/protocol_fixtures.rs — if you change a value
// here, change it there too. Values that api.go takes from the clock
// (CreateAlbum / AddMediaToAlbum timestamps) are pinned to
// fixtureTimestamp so the bytes are reproducible; api.go's now() would
// produce different bytes on every run.
package main

import (
	"encoding/hex"
	"fmt"
	"os"

	"app/generated"

	"google.golang.org/protobuf/proto"
)

// Fixed inputs shared with the Rust tests.
var (
	// SHA-1 of the empty string — a recognisable, fixed 20-byte hash.
	fixtureSHA1 = []byte{
		0xda, 0x39, 0xa3, 0xee, 0x5e, 0x6b, 0x4b, 0x0d, 0x32, 0x55,
		0xbf, 0xef, 0x95, 0x60, 0x18, 0x90, 0xaf, 0xd8, 0x07, 0x09,
	}
	fixtureOpaqueToken   = []byte{0xde, 0xad, 0xbe, 0xef}
	fixtureFileName      = "IMG_0001.jpg"
	fixtureFileSize      = int64(123456789)
	fixtureTimestamp     = int64(1700000000)
	fixtureUnknownInt    = int64(46000000) // core/api.go CommitUpload: unknownInt
	fixtureAlbumName     = "Test Album"
	fixtureAlbumKey      = "ALBUMKEY"
	fixtureMediaKey      = "MEDIAKEY1"
	fixtureMediaKey2     = "MEDIAKEY2"
	fixtureDeviceModel   = "Pixel XL"
	fixtureDeviceModelSv = "Pixel 2" // saver mode (core/api.go CommitUpload)
	fixtureDeviceMake    = "Google"
	fixtureAndroidAPI    = int64(28)
)

func emit(name string, message proto.Message) {
	data, err := proto.Marshal(message)
	if err != nil {
		fmt.Fprintf(os.Stderr, "marshal %s: %v\n", name, err)
		os.Exit(1)
	}
	fmt.Printf("%s=%s\n", name, hex.EncodeToString(data))
}

func deviceInfo() *generated.CommitUploadField2Type {
	return &generated.CommitUploadField2Type{
		Model:             fixtureDeviceModel,
		Make:              fixtureDeviceMake,
		AndroidApiVersion: fixtureAndroidAPI,
	}
}

func commitUpload(quality int64, model string) *generated.CommitUpload {
	device := deviceInfo()
	device.Model = model
	// Mirrors core/api.go CommitUpload field-for-field.
	return &generated.CommitUpload{
		Field1: &generated.CommitUploadField1Type{
			Field1: &generated.CommitUploadField1TypeField1Type{
				Field1: 2, // Scotty finalize token field 1 (its version value)
				Field2: fixtureOpaqueToken,
			},
			FileName: fixtureFileName,
			Sha1Hash: fixtureSHA1,
			Field4: &generated.CommitUploadField1TypeField4Type{
				FileLastModifiedTimestamp: fixtureTimestamp,
				Field2:                    fixtureUnknownInt,
			},
			Quality: quality,
			Field10: 1,
		},
		Field2: device,
		Field3: []byte{1, 3},
	}
}

func main() {
	// Mirrors core/api.go GetUploadToken.
	emit("get_upload_token", &generated.GetUploadToken{
		F1:            2,
		F2:            2,
		F3:            1,
		F4:            3,
		FileSizeBytes: fixtureFileSize,
	})

	// Mirrors core/api.go FindRemoteMediaByHash.
	emit("hash_check", &generated.HashCheck{
		Field1: &generated.HashCheckField1Type{
			Field1: &generated.HashCheckField1TypeField1Type{
				Sha1Hash: fixtureSHA1,
			},
			Field2: &generated.HashCheckField1TypeField2Type{},
		},
	})

	// The Scotty finalize token / legacy CommitToken (core/scotty_token.go):
	// field 1 is the version (must be exactly 2, exactly once), field 2 is
	// the opaque payload (non-empty, exactly once).
	emit("commit_token", &generated.CommitToken{
		Field1: 2,
		Field2: fixtureOpaqueToken,
	})

	// Mirrors core/api.go CommitUpload (default and Saver quality).
	emit("commit_upload", commitUpload(3, fixtureDeviceModel))
	emit("commit_upload_saver", commitUpload(1, fixtureDeviceModelSv))

	// Mirrors core/api.go CreateAlbum, with the timestamp pinned.
	emit("create_album", &generated.CreateAlbum{
		AlbumName: fixtureAlbumName,
		Timestamp: fixtureTimestamp,
		Field3:    1,
		MediaKeys: []*generated.CreateAlbumField4Type{
			{Field1: &generated.CreateAlbumField4TypeField1Type{MediaKey: fixtureMediaKey}},
		},
		Field6: &generated.CreateAlbumField6Type{},
		Field7: &generated.CreateAlbumField7Type{Field1: 3},
		DeviceInfo: &generated.CreateAlbumField8Type{
			Model:             fixtureDeviceModel,
			Make:              fixtureDeviceMake,
			AndroidApiVersion: fixtureAndroidAPI,
		},
	})

	// Mirrors core/api.go AddMediaToAlbum, with the timestamp pinned.
	emit("add_media_to_album", &generated.AddMediaToAlbum{
		MediaKeys:     []string{fixtureMediaKey, fixtureMediaKey2},
		AlbumMediaKey: fixtureAlbumKey,
		Field5:        &generated.AddMediaToAlbumField5Type{Field1: 2},
		DeviceInfo: &generated.AddMediaToAlbumField6Type{
			Model:             fixtureDeviceModel,
			Make:              fixtureDeviceMake,
			AndroidApiVersion: fixtureAndroidAPI,
		},
		Timestamp: fixtureTimestamp,
	})

	// Response fixtures (messages the Rust side must DECODE): built here so
	// the decode tests also run against genuine generated-code bytes.

	// generated/utils.go GetMediaKey path: Field1.Field2.Field2.MediaKey.
	emit("remote_matches", &generated.RemoteMatches{
		Field1: &generated.RemoteMatchesField1Type{
			Field2: &generated.RemoteMatchesField1TypeField2Type{
				Field1: &generated.RemoteMatchesField1TypeField2TypeField1Type{
					Sha1Hash: fixtureSHA1,
				},
				Field2: &generated.RemoteMatchesField1TypeField2TypeField2Type{
					MediaKey: fixtureMediaKey,
				},
			},
		},
	})

	// core/api.go parseCreateMediaItemsResponse path:
	// item[].result_item.media_key.
	emit("create_media_items_response", &generated.CreateMediaItemsResponse{
		Item: []*generated.CreateMediaItemResponseItem{
			{ResultItem: &generated.CreateMediaItemResult{MediaKey: fixtureMediaKey}},
		},
	})

	// core/api.go CreateAlbum response path: field1.album_media_key.
	emit("create_album_response", &generated.CreateAlbumResponse{
		Field1: &generated.CreateAlbumResponseField1Type{
			AlbumMediaKey: fixtureAlbumKey,
		},
	})
}
