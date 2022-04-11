package alephium

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/go-test/deep"
	"github.com/stretchr/testify/assert"
	"go.uber.org/zap"
)

func TestSubscribeEvents(t *testing.T) {
	contractAddress := randomAddress()
	eventCount := uint64(0)

	event0 := &Event{
		BlockHash:       randomByte32().ToHex(),
		ContractAddress: contractAddress,
		TxId:            randomByte32().ToHex(),
		Index:           0,
		Fields: []*Field{
			{
				Type:  "U256",
				Value: "0",
			},
		},
	}

	event1 := &Event{
		BlockHash:       randomByte32().ToHex(),
		ContractAddress: contractAddress,
		TxId:            randomByte32().ToHex(),
		Index:           0,
		Fields: []*Field{
			{
				Type:  "U256",
				Value: "2",
			},
		},
	}

	// event from orphan block
	event2 := &Event{
		BlockHash:       randomByte32().ToHex(),
		ContractAddress: contractAddress,
		TxId:            randomByte32().ToHex(),
		Index:           0,
		Fields: []*Field{
			{
				Type:  "U256",
				Value: "0",
			},
		},
	}

	events := make([]*Event, 0)
	isCanonicalBlock := uint32(1)

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.RequestURI == eventCountURI(contractAddress) {
			w.Header().Set("Content-Type", "application/json")
			json.NewEncoder(w).Encode(len(events))
			return
		}

		if strings.HasPrefix(r.RequestURI, "/events/contract?start=") {
			w.Header().Set("Content-Type", "application/json")
			query := r.URL.Query()
			from, err := strconv.Atoi(query["start"][0])
			assert.Nil(t, err)
			to, err := strconv.Atoi(query["end"][0])
			assert.Nil(t, err)
			json.NewEncoder(w).Encode(&Events{
				ChainFrom: 0,
				ChainTo:   0,
				Events:    events[from : to+1],
			})
			return
		}

		if strings.HasPrefix(r.RequestURI, "/blockflow/is-block-in-main-chain") {
			w.Header().Set("Content-Type", "application/json")
			json.NewEncoder(w).Encode(atomic.LoadUint32(&isCanonicalBlock) == 1)
			return
		}
	}))

	ctx, cancel := context.WithCancel(context.Background())
	logger := zap.NewNop()
	client := NewClient(server.URL, "", 10)
	errC := make(chan error)
	watcher := &Watcher{
		currentHeight: 0,
	}

	toUnconfirmed := func(ctx context.Context, client *Client, event *Event) (*UnconfirmedEvent, error) {
		confirmations, err := event.Fields[0].ToUint8()
		assert.Nil(t, err)
		return &UnconfirmedEvent{
			blockHeader: &BlockHeader{
				Height: 0,
			},
			event:         event,
			confirmations: confirmations,
		}, nil
	}

	confirmedEvents := make([]*ConfirmedEvents, 0)
	handler := func(confirmed *ConfirmedEvents) error {
		confirmedEvents = append(confirmedEvents, confirmed)
		return nil
	}

	go watcher.subscribe_(ctx, logger, client, contractAddress, eventCount, toUnconfirmed, handler, 500*time.Millisecond, errC)

	time.Sleep(1 * time.Second)
	assert.True(t, len(confirmedEvents) == 0)

	// event0 confirmed
	atomic.StoreUint32(&watcher.currentHeight, 1)
	events = append(events, event0)
	time.Sleep(1 * time.Second)
	assert.True(t, len(confirmedEvents) == 1)
	assert.True(t, len(confirmedEvents[0].events) == 1)
	diff := deep.Equal(confirmedEvents[0].events[0].event, event0)
	assert.Nil(t, diff)

	// event1 not confirmed
	events = append(events, event1)
	time.Sleep(1 * time.Second)
	assert.True(t, len(confirmedEvents) == 1)

	// event1 confirmed
	atomic.StoreUint32(&watcher.currentHeight, 3)
	time.Sleep(1 * time.Second)
	assert.True(t, len(confirmedEvents) == 2)
	assert.True(t, len(confirmedEvents[1].events) == 1)
	diff = deep.Equal(confirmedEvents[1].events[0].event, event1)
	assert.Nil(t, diff)

	// event2
	atomic.StoreUint32(&isCanonicalBlock, 0)
	events = append(events, event2)
	time.Sleep(1 * time.Second)
	assert.True(t, len(confirmedEvents) == 2)

	cancel()
}

func TestUpdateTokenBridgeForChain(t *testing.T) {
	db, err := Open(t.TempDir())
	assert.Nil(t, err)
	defer db.Close()

	watcher := &Watcher{
		chainIndex: &ChainIndex{
			FromGroup: 0,
			ToGroup:   0,
		},
		db:                       db,
		tokenBridgeForChainCache: sync.Map{},
	}

	contractAddresses := []string{
		randomAddress(), randomAddress(), randomAddress(),
	}
	tokenBridgeForChains := make(map[string]*ContractState)
	var confirmedEvents ConfirmedEvents
	for i := 0; i < 3; i++ {
		address := contractAddresses[i]
		tokenBridgeForChains[address] = &ContractState{
			Address: address,
			Fields: []*Field{
				{}, {},
				{
					Type:  "U256",
					Value: fmt.Sprintf("%d", i),
				},
			},
		}

		confirmedEvents.events = append(confirmedEvents.events, &UnconfirmedEvent{
			event: &Event{
				Fields: []*Field{
					{
						Type:  "Address",
						Value: address,
					},
				},
			},
		})
	}
	confirmedEvents.events[0].eventIndex = 2
	confirmedEvents.events[1].eventIndex = 1
	confirmedEvents.events[2].eventIndex = 3

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		parts := strings.Split(r.URL.Path, "/")
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(tokenBridgeForChains[parts[2]])
	}))

	client := NewClient(server.URL, "", 10)
	err = watcher.updateTokenBridgeForChain(context.Background(), zap.NewNop(), client, &confirmedEvents)
	assert.Nil(t, err)

	eventIndex, err := watcher.db.getLastTokenBridgeEventIndex()
	assert.Nil(t, err)
	assert.Equal(t, *eventIndex, uint64(3))

	for i := 0; i < 3; i++ {
		address, err := watcher.db.getRemoteChain(uint16(i))
		assert.Nil(t, err)
		assert.Equal(t, address, contractAddresses[i])

		contractId, ok := watcher.tokenBridgeForChainCache.Load(uint16(i))
		assert.True(t, ok)
		assert.Equal(t, *contractId.(*Byte32), toContractId(contractAddresses[0]))
	}
}