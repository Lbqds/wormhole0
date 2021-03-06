import { CliqueClient, Number256 } from "alephium-web3"
import { createMath } from "./fixtures/wormhole-fixture"

describe('test math', () => {
    const client = new CliqueClient({baseUrl: `http://127.0.0.1:22973`})

    interface TestCase {
        decimals: number
        amount: bigint
        normalizedAmount: bigint
        deNormalizedAmount: bigint
    }

    it('should test math methods', async () => {
        const mathInfo = await createMath(client)
        const contract = mathInfo.contract

        const cases: TestCase[] = [
            {
                decimals: 6,
                amount: BigInt("100000"),
                normalizedAmount: BigInt("100000"), 
                deNormalizedAmount: BigInt("100000")
            },
            {
                decimals: 8,
                amount: BigInt("10000000"),
                normalizedAmount: BigInt("10000000"),
                deNormalizedAmount: BigInt("10000000")
            },
            {
                decimals: 10,
                amount: BigInt("10000000011"),
                normalizedAmount: BigInt("100000000"),
                deNormalizedAmount: BigInt("10000000000")
            }
        ]
        cases.forEach(async tc => {
            let testResult = await contract.testPublicMethod(client, 'normalizeAmount', {
                testArgs: [tc.amount, tc.decimals]
            })
            expect(testResult.returns.length).toEqual(1)
            const normalizedAmount = testResult.returns[0] as Number256
            expect(normalizedAmount).toEqual(Number(tc.normalizedAmount))

            testResult = await contract.testPublicMethod(client, 'deNormalizeAmount', {
                testArgs: [tc.normalizedAmount, tc.decimals]
            })
            expect(testResult.returns.length).toEqual(1)
            const deNormalizedAmount = testResult.returns[0] as Number256
            expect(deNormalizedAmount).toEqual(Number(tc.deNormalizedAmount))
        })
    })
})