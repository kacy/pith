// hash_kernels: go counterpart of bench/hash_kernels.pith.
//
// fills a buffer with xorshift64 output written eight bytes at a time,
// little-endian, then runs one hash or checksum over it and prints the
// digest, which matches the pith and rust versions. every kernel comes from
// the go standard library: crypto/sha1, crypto/sha256 (sha224 too),
// crypto/sha512 (sha384 too), hash/fnv (32-bit fnv-1a), hash/crc32 (ieee) and
// hash/adler32. the sha and crc32 kernels use assembly and cpu extensions
// where the machine has them; the pith kernels are portable code.
//
// build: go build -o bench/hash_kernels_go bench/hash_kernels.go
// run:   ./bench/hash_kernels_go sha256 256
//        ./bench/hash_kernels_go none 256    # generation only
package main

import (
	"crypto/sha1"
	"crypto/sha256"
	"crypto/sha512"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"hash/adler32"
	"hash/crc32"
	"hash/fnv"
	"os"
	"strconv"
)

func main() {
	kernel := "sha256"
	mb := 4
	if len(os.Args) > 1 {
		kernel = os.Args[1]
	}
	if len(os.Args) > 2 {
		if v, err := strconv.Atoi(os.Args[2]); err == nil {
			mb = v
		}
	}
	total := mb * 1048576
	data := make([]byte, 0, total)
	state := uint64(88172645463325252)
	for len(data) < total {
		state ^= state << 13
		state ^= state >> 7
		state ^= state << 17
		data = binary.LittleEndian.AppendUint64(data, state)
	}
	fmt.Printf("len=%d\n", len(data))
	switch kernel {
	case "none":
		fmt.Println("digest=none")
	case "sha1":
		sum := sha1.Sum(data)
		fmt.Println("digest=" + hex.EncodeToString(sum[:]))
	case "sha224":
		sum := sha256.Sum224(data)
		fmt.Println("digest=" + hex.EncodeToString(sum[:]))
	case "sha256":
		sum := sha256.Sum256(data)
		fmt.Println("digest=" + hex.EncodeToString(sum[:]))
	case "sha384":
		sum := sha512.Sum384(data)
		fmt.Println("digest=" + hex.EncodeToString(sum[:]))
	case "sha512":
		sum := sha512.Sum512(data)
		fmt.Println("digest=" + hex.EncodeToString(sum[:]))
	case "fnv1a":
		h := fnv.New32a()
		h.Write(data)
		fmt.Printf("digest=%d\n", h.Sum32())
	case "crc32":
		fmt.Printf("digest=%d\n", crc32.ChecksumIEEE(data))
	case "adler32":
		fmt.Printf("digest=%d\n", adler32.Checksum(data))
	default:
		fmt.Println("unknown kernel " + kernel + "; one of none sha1 sha224 sha256 sha384 sha512 fnv1a crc32 adler32")
		os.Exit(2)
	}
}
