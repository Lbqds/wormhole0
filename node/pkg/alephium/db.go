package alephium

import (
	"encoding/binary"
	"fmt"

	"github.com/dgraph-io/badger/v3"
)

type db struct {
	*badger.DB
}

var (
	tokenWrapperPrefix = []byte("token-wrapper")
	chainPrefix        = []byte("token-bridge-for-chain")
	lastBlockHeightKey = []byte("last-block-height")
)

func open(path string) (*db, error) {
	database, err := badger.Open(badger.DefaultOptions(path))
	if err != nil {
		return nil, fmt.Errorf("failed to open database: %w", err)
	}
	return &db{
		database,
	}, nil
}

func (db *db) put(key []byte, value []byte) error {
	return db.Update(func(txn *badger.Txn) error {
		return txn.Set(key, value)
	})
}

func (db *db) get(key []byte) (b []byte, err error) {
	err = db.View(func(txn *badger.Txn) error {
		item, err := txn.Get(key)
		if err != nil {
			return err
		}
		val, err := item.ValueCopy(nil)
		if err != nil {
			return err
		}
		b = val
		return nil
	})
	return
}

func (db *db) addTokenWrapper(tokenId Byte32, tokenWrapperAddress string) error {
	return db.put(tokenWrapperKey(tokenId), []byte(tokenWrapperAddress))
}

func (db *db) getTokenWrapper(tokenId Byte32) (string, error) {
	value, err := db.get(tokenWrapperKey(tokenId))
	if err != nil {
		return "", err
	}
	return string(value), nil
}

func (db *db) addRemoteChain(chainId uint16, tokenBridgeForChainAddress string) error {
	return db.put(chainKey(chainId), []byte(tokenBridgeForChainAddress))
}

func (db *db) getRemoteChain(chainId uint16) (string, error) {
	value, err := db.get(chainKey(chainId))
	if err != nil {
		return "", err
	}
	return string(value), nil
}

func (db *db) updateLastHeight(height uint32) error {
	bytes := make([]byte, 4)
	binary.BigEndian.PutUint32(bytes, height)
	return db.put(lastBlockHeightKey, bytes)
}

func (db *db) getLastHeight() (uint32, error) {
	value, err := db.get(lastBlockHeightKey)
	if err != nil {
		return 0, err
	}
	return binary.BigEndian.Uint32(value), nil
}

func (db *db) writeBatch(batch *batch) error {
	return db.Update(func(txn *badger.Txn) error {
		for i, key := range batch.keys {
			if err := txn.Set(key, batch.values[i]); err != nil {
				return err
			}
		}
		return nil
	})
}

func tokenWrapperKey(tokenId Byte32) []byte {
	return append(tokenWrapperPrefix, tokenId[:]...)
}

func chainKey(chainId uint16) []byte {
	bytes := make([]byte, 2)
	binary.BigEndian.PutUint16(bytes, chainId)
	return append(chainPrefix, bytes...)
}

type batch struct {
	keys   [][]byte
	values [][]byte
}

func newBatch() *batch {
	return &batch{
		keys:   make([][]byte, 0),
		values: make([][]byte, 0),
	}
}

func (b *batch) writeChain(chainId uint16, contractAddress string) {
	b.keys = append(b.keys, chainKey(chainId))
	b.values = append(b.values, []byte(contractAddress))
}

func (b *batch) writeTokenWrapper(tokenId Byte32, wrapperAddress string) {
	b.keys = append(b.keys, tokenWrapperKey(tokenId))
	b.values = append(b.values, []byte(wrapperAddress))
}

func (b *batch) updateHeight(height uint32) {
	b.keys = append(b.keys, lastBlockHeightKey)
	bytes := make([]byte, 4)
	binary.BigEndian.PutUint32(bytes, height)
	b.values = append(b.values, bytes)
}
