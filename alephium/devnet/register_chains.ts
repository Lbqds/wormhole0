import { CliqueClient, Script, Signer } from 'alephium-js'
import { deploySequence, Wormhole } from '../lib/wormhole'
import * as env from './env'

export interface RemoteChains {
    eth: string,
    terra: string,
    solana: string,
    bsc: string
}

export async function registerChains(wormhole: Wormhole, tokenBridgeAddress: string): Promise<RemoteChains> {
    const payer = "12LgGdbjE6EtnTKw5gdBwV2RRXuXPtzYM7SDZ45YJTRht"
    const alphAmount = BigInt("1000000000000000000")
    const vaas = [
        // ETH, sequence = 0
        '01000000000100e2e1975d14734206e7a23d90db48a6b5b6696df72675443293c6057dcb936bf224b5df67d32967adeb220d4fe3cb28be515be5608c74aab6adb31099a478db5c1c000000010000000100010000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000546f6b656e42726964676501000000020000000000000000000000000290fb167208af455bb137780163b7b7a9a10c16',
        // Terra, sequence = 1
        '01000000000100e7d8469492e85b4f0df03a8bc1cdbf395f843ea181fb47188fcbf67b6df621fd005fad7ae7215752f0bfc6ff0f894dc4793cb46428e7582a369cfaeb05f334f31b000000010000000100010000000000000000000000000000000000000000000000000000000000000004000000000000000100000000000000000000000000000000000000000000546f6b656e4272696467650100000003000000000000000000000000784999135aaa8a3ca5914468852fdddbddd8789d',
        // Solana, sequence = 2
        '010000000001001501232cc660aab7e2a84099fa7823048c2cab4834b1c3579656bd02b8686134150d3d69ab5bfda5aec1aa53bb3fcd89969462faa3cfd08551b36ed45b1202851c000000010000000100010000000000000000000000000000000000000000000000000000000000000004000000000000000200000000000000000000000000000000000000000000546f6b656e4272696467650100000001c69a1b1a65dd336bf1df6a77afb501fc25db7fc0938cb08595a9ef473265cb4f',
        // BSC, sequence = 3
        '01000000000100f2766a939e1cde40d3a39218c4eaac273469f3e05edb7e55c0897eb7d565432550cbbec75013abfa30a3160d3915ffa7b6232c7062ea5fd8db62ba6bff6928691c000000010000000100010000000000000000000000000000000000000000000000000000000000000004000000000000000300000000000000000000000000000000000000000000546f6b656e42726964676501000000040000000000000000000000000290fb167208af455bb137780163b7b7a9a10c16'
    ]
    const params = {
        alphAmount: alphAmount,
        gas: 500000
    }

    var txId = await wormhole.registerChainToAlph(tokenBridgeAddress, vaas[0], payer, env.dustAmount, params)
    const bridgeForEth = "24N8dYt8zwhpDbzVLQiMLYYBYNfzujoAPV17zDe8kYTd3"
    console.log("register eth txId: " + txId)
    txId = await wormhole.registerChainToAlph(tokenBridgeAddress, vaas[1], payer, env.dustAmount, params)
    const bridgeForTerra = "xqQXvmp8pRpo3hao2a57kNYqm5qN4zcGN4YbMJCNwwgP"
    console.log("register terra txId: " + txId)
    txId = await wormhole.registerChainToAlph(tokenBridgeAddress, vaas[2], payer, env.dustAmount, params)
    const bridgeForSolana = "zY1TdLLNBzGmcJrgiqs52ESzsckqye1Mys6yjMZwvubX"
    console.log("register solana txId: " + txId)
    txId = await wormhole.registerChainToAlph(tokenBridgeAddress, vaas[3], payer, env.dustAmount, params)
    const bridgeForBsc = "2Bg3TvRG7XU8FNHC5G2cevvHzXj5AjJSP3Ec2Rajw1pyE"
    console.log("register bsc txId: " + txId)

    await initTokenBridgeForChain(wormhole.client, wormhole.signer, bridgeForEth)
    await initTokenBridgeForChain(wormhole.client, wormhole.signer, bridgeForTerra)
    await initTokenBridgeForChain(wormhole.client, wormhole.signer, bridgeForSolana)
    await initTokenBridgeForChain(wormhole.client, wormhole.signer, bridgeForBsc)

    return {
        eth: bridgeForEth,
        terra: bridgeForTerra,
        solana: bridgeForSolana,
        bsc: bridgeForBsc
    }
}

async function initTokenBridgeForChain(
    client: CliqueClient,
    signer: Signer,
    address: string
) {
    const deployResult = await deploySequence(client, signer, address)
    const script = await Script.from(client, 'token_bridge_for_chain_init.ral', {
        tokenBridgeForChainAddress: address,
        sequenceAddress: deployResult.address,
        serdeAddress: "00",
        tokenWrapperFactoryAddress: "00",
        tokenWrapperCodeHash: "00",
        tokenWrapperBinCode: "00",
        tokenBridgeForChainBinCode: "00",
        tokenBridgeForChainCodeHash: "00",
        sequenceCodeHash: "00"
    })
    const scriptTx = await script.transactionForDeployment(signer)
    await signer.submitTransaction(scriptTx.unsignedTx, scriptTx.txId)
}
