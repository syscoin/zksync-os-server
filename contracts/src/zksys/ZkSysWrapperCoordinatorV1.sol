// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {EIP712} from "@openzeppelin/contracts-v4/utils/cryptography/EIP712.sol";
import {SignatureChecker} from "@openzeppelin/contracts-v4/utils/cryptography/SignatureChecker.sol";
import {MerkleProof} from "@openzeppelin/contracts-v4/utils/cryptography/MerkleProof.sol";
import {
    AcceptedPackageV1,
    WrapperCandidateV1,
    IZkSysAuthenticatedRootSourceV1,
    IZkSysNativeRootDrawV1,
    IZkSysL1MessengerV1,
    IZkSysWrapperRosterSourceV1,
    ZkSysServiceTypesV1
} from "./ZkSysServiceTypesV1.sol";

/// @notice Freezes each qualified roster before root entropy selects its wrapper schedule.
/// @dev The immutable gate must verify the production proof before calling acceptPackage. This is
/// endorsement authorization, not a native-proof verifier. The root source authenticates relayed
/// block hashes; this does not make PoW randomness unbiased or establish ChainLock finality on chain.
contract ZkSysWrapperCoordinatorV1 is EIP712 {
    using SignatureChecker for address;

    bytes32 public constant DRAW_DOMAIN = keccak256("ZKSYS_WRAPPER_DRAW_V1");
    bytes32 public constant ROSTER_DRAW_DOMAIN = keccak256("ZKSYS_WRAPPER_ROSTER_DRAW_V1");
    IZkSysL1MessengerV1 public constant L1_MESSENGER = IZkSysL1MessengerV1(address(0x8008));

    struct Configuration {
        address acceptanceGate;
        IZkSysAuthenticatedRootSourceV1 rootSource;
        IZkSysWrapperRosterSourceV1 rosterSource;
        address sequencer;
        uint256 childChainId;
        address childChainAddress;
        bytes32 policyHash;
        bytes32 productionVkHash;
        bytes32 initialParent;
        uint64 firstBatch;
        uint64 firstServicePeriod;
        uint64 turnSeconds;
        bool nativeRootDraw;
    }

    struct RosterDraw {
        bytes32 root;
        uint32 count;
        bytes32 commitment;
        uint64 height;
        bytes32 seed;
    }

    error InvalidConfiguration();
    error UnauthorizedGate();
    error PackageAlreadyOpen();
    error NoOpenPackage();
    error InvalidPackage();
    error InvalidRoster();
    error RootNotAvailable();
    error TurnsNotStarted();
    error WrongWrapper();
    error InvalidSignature();

    address public immutable acceptanceGate;
    IZkSysAuthenticatedRootSourceV1 public immutable rootSource;
    IZkSysWrapperRosterSourceV1 public immutable rosterSource;
    address public immutable sequencer;
    uint256 public immutable childChainId;
    address public immutable childChainAddress;
    bytes32 public immutable policyHash;
    bytes32 public immutable productionVkHash;
    uint64 public immutable turnSeconds;
    uint64 public immutable firstServicePeriod;
    bool public immutable nativeRootDraw;

    bytes32 public acceptedParent;
    uint64 public nextBatch;
    bool public packageOpen;
    bytes32 public frozenPackageHash;
    uint64 public rootHeight;
    uint64 public turnsStartedAt;
    uint32 public firstWrapperIndex;
    uint32 public rosterCount;
    bytes32 public rosterRoot;
    uint64 public packageOrdinal;
    mapping(uint64 period => RosterDraw draw) private _rosterDraws;
    bool private _hasPastReady;
    bool private _hasFutureReady;
    uint64 private _pastReadyPeriod;
    uint64 private _futureReadyPeriod;
    AcceptedPackageV1 private _frozenPackage;

    event PackageOpened(bytes32 indexed packageHash, uint64 indexed rootHeight, bytes32 rosterRoot, uint32 rosterCount);
    event WrapperTurnsStarted(bytes32 indexed packageHash, uint32 firstWrapperIndex, uint64 startedAt);
    event RosterDrawPrepared(uint64 indexed period, bytes32 indexed commitment, bytes32 root, uint32 count);
    event RosterDrawRecorded(uint64 indexed period, uint64 rootHeight, bytes32 seed);
    event PackageAccepted(bytes32 indexed packageHash, uint64 indexed batchTo, address indexed wrapper, uint32 turn);
    event PackageRepaired(bytes32 indexed previousHash, bytes32 indexed replacementHash);

    constructor(Configuration memory config) EIP712("ZkSysWrapperCoordinator", "1") {
        if (
            config.acceptanceGate.code.length == 0 || address(config.rootSource).code.length == 0
                || address(config.rosterSource).code.length == 0 || config.sequencer == address(0)
                || config.childChainId == 0 || config.childChainAddress == address(0) || config.policyHash == bytes32(0)
                || config.productionVkHash == bytes32(0) || config.initialParent == bytes32(0) || config.firstBatch == 0
                || config.turnSeconds == 0
        ) revert InvalidConfiguration();
        if (
            config.rosterSource.firstServicePeriod() != config.firstServicePeriod
                || config.rosterSource.periodSeconds() == 0
                || (config.nativeRootDraw
                    && IZkSysNativeRootDrawV1(address(config.rootSource)).coordinator() != address(this))
        ) revert InvalidConfiguration();
        acceptanceGate = config.acceptanceGate;
        rootSource = config.rootSource;
        rosterSource = config.rosterSource;
        sequencer = config.sequencer;
        childChainId = config.childChainId;
        childChainAddress = config.childChainAddress;
        policyHash = config.policyHash;
        productionVkHash = config.productionVkHash;
        acceptedParent = config.initialParent;
        nextBatch = config.firstBatch;
        turnSeconds = config.turnSeconds;
        firstServicePeriod = config.firstServicePeriod;
        nativeRootDraw = config.nativeRootDraw;
    }

    modifier onlyGate() {
        if (msg.sender != acceptanceGate) revert UnauthorizedGate();
        _;
    }

    function prepareRosterDraw(uint64 period) external returns (bytes32 commitment) {
        if (period < firstServicePeriod) revert InvalidRoster();
        RosterDraw storage draw = _rosterDraws[period];
        if (draw.commitment != bytes32(0)) return draw.commitment;
        (bytes32 root, uint32 count) = rosterSource.rosterFor(period);
        if (root == bytes32(0) || count == 0) revert InvalidRoster();
        commitment = keccak256(
            abi.encode(
                ROSTER_DRAW_DOMAIN,
                block.chainid,
                address(this),
                childChainId,
                childChainAddress,
                policyHash,
                period,
                root,
                count
            )
        );
        draw.root = root;
        draw.count = count;
        draw.commitment = commitment;
        if (nativeRootDraw) IZkSysNativeRootDrawV1(address(rootSource)).requestDraw(commitment);
        else L1_MESSENGER.sendToL1(abi.encode(DRAW_DOMAIN, commitment));
        emit RosterDrawPrepared(period, commitment, root, count);
    }

    function recordRosterDraw(uint64 period) external {
        uint64 current = _clockPeriod();
        if (uint256(period) > uint256(current) + 1) revert InvalidRoster();
        RosterDraw storage draw = _rosterDraws[period];
        if (draw.commitment == bytes32(0)) revert InvalidRoster();
        if (draw.seed != bytes32(0)) return;
        (uint64 height, bytes32 rootHash) = rootSource.drawFor(draw.commitment);
        if (height == 0 || rootHash == bytes32(0)) revert RootNotAvailable();
        draw.height = height;
        draw.seed = keccak256(abi.encode(draw.commitment, height, rootHash));
        if (_hasFutureReady && _futureReadyPeriod <= current) {
            if (!_hasPastReady || _futureReadyPeriod > _pastReadyPeriod) {
                _pastReadyPeriod = _futureReadyPeriod;
                _hasPastReady = true;
            }
            _hasFutureReady = false;
        }
        if (period <= current) {
            if (!_hasPastReady || period > _pastReadyPeriod) {
                _pastReadyPeriod = period;
                _hasPastReady = true;
            }
        } else {
            _futureReadyPeriod = period;
            _hasFutureReady = true;
        }
        emit RosterDrawRecorded(period, height, draw.seed);
    }

    function rosterDrawCommitment(uint64 period) external view returns (bytes32) {
        return _rosterDraws[period].commitment;
    }

    function rosterSeed(uint64 period) external view returns (bytes32) {
        return _rosterDraws[period].seed;
    }

    function _clockPeriod() private view returns (uint64 period) {
        uint256 start = rosterSource.startTime();
        uint256 current =
            block.timestamp < start ? firstServicePeriod : (block.timestamp - start) / rosterSource.periodSeconds();
        if (current < firstServicePeriod) current = firstServicePeriod;
        if (current > type(uint64).max) revert InvalidRoster();
        return uint64(current);
    }

    /// @dev A ready current roster always wins. The bounded cache retains the greatest ready past
    /// period plus one next period; recording future entropy cannot hide a usable recovery roster.
    function openingRoster() public view returns (uint64 period, bytes32 root, uint32 count, bool controlWork) {
        period = _clockPeriod();
        uint256 firstStart = rosterSource.startTime() + uint256(firstServicePeriod) * rosterSource.periodSeconds();
        (root, count) = rosterSource.rosterFor(period);
        RosterDraw storage current = _rosterDraws[period];
        if (
            root != bytes32(0) && count != 0 && current.root == root && current.count == count
                && current.seed != bytes32(0)
        ) {
            return (period, root, count, block.timestamp < firstStart);
        }
        bool found = _hasPastReady;
        uint64 fallbackPeriod = _pastReadyPeriod;
        if (_hasFutureReady && _futureReadyPeriod <= period && (!found || _futureReadyPeriod > fallbackPeriod)) {
            found = true;
            fallbackPeriod = _futureReadyPeriod;
        }
        if (!found || fallbackPeriod >= period) revert RootNotAvailable();
        RosterDraw storage previous = _rosterDraws[fallbackPeriod];
        return (fallbackPeriod, previous.root, previous.count, true);
    }

    function openPackage(AcceptedPackageV1 calldata proposed) external onlyGate {
        if (packageOpen) revert PackageAlreadyOpen();
        (uint64 period, bytes32 root, uint32 count,) = openingRoster();
        if (period < firstServicePeriod || root == bytes32(0) || count == 0) revert InvalidRoster();
        RosterDraw storage draw = _rosterDraws[period];
        if (draw.root != root || draw.count != count) revert InvalidRoster();
        if (draw.seed == bytes32(0)) revert RootNotAvailable();
        if (
            proposed.domainVersion != ZkSysServiceTypesV1.DOMAIN_VERSION || proposed.policyHash != policyHash
                || proposed.chainId != childChainId || proposed.chainAddress != childChainAddress
                || proposed.parent != acceptedParent || proposed.batchFrom != nextBatch
                || proposed.batchTo <= proposed.batchFrom
                || proposed.batchTo - proposed.batchFrom >= ZkSysServiceTypesV1.MAX_BATCHES_PER_PACKAGE
                || proposed.batchTo == type(uint64).max || proposed.protocolVersion != 32
                || proposed.vkHash != productionVkHash || proposed.period != period || proposed.rosterRoot != root
                || proposed.sequencer != sequencer || proposed.sequencerBeneficiary == address(0)
                || proposed.manifestHash == bytes32(0) || proposed.reportHash == bytes32(0) || proposed.turn != 0
                || proposed.proofHash != bytes32(0) || proposed.wrapper != address(0)
                || proposed.wrapperBeneficiary != address(0)
        ) revert InvalidPackage();

        if (block.timestamp == 0 || block.timestamp > type(uint64).max) revert InvalidConfiguration();
        // Only accepting canonical work advances the ordinal. Changing a manifest, range size,
        // submission time, or retry cannot buy a different draw for this pending package.
        rootHeight = draw.height;
        _frozenPackage = proposed;
        frozenPackageHash = ZkSysServiceTypesV1.hashPackage(proposed);
        rosterCount = count;
        rosterRoot = root;
        firstWrapperIndex = uint32(uint256(keccak256(abi.encode(draw.seed, packageOrdinal))) % count);
        turnsStartedAt = uint64(block.timestamp);
        packageOpen = true;
        emit PackageOpened(frozenPackageHash, rootHeight, root, count);
        emit WrapperTurnsStarted(frozenPackageHash, firstWrapperIndex, turnsStartedAt);
    }

    function currentTurn() public view returns (uint32) {
        if (!packageOpen) revert NoOpenPackage();
        if (turnsStartedAt == 0) revert TurnsNotStarted();
        uint256 turn = (block.timestamp - turnsStartedAt) / turnSeconds;
        if (turn > type(uint32).max) revert InvalidConfiguration();
        return uint32(turn);
    }

    function repairPackage(AcceptedPackageV1 calldata proposed) external onlyGate {
        if (!packageOpen) revert NoOpenPackage();
        AcceptedPackageV1 memory expected = _frozenPackage;
        if (
            proposed.batchTo <= expected.batchFrom
                || proposed.batchTo - expected.batchFrom >= ZkSysServiceTypesV1.MAX_BATCHES_PER_PACKAGE
                || proposed.batchTo == type(uint64).max || proposed.manifestHash == bytes32(0)
                || proposed.reportHash == bytes32(0)
        ) revert InvalidPackage();
        expected.batchTo = proposed.batchTo;
        expected.manifestHash = proposed.manifestHash;
        expected.reportHash = proposed.reportHash;
        bytes32 replacement = ZkSysServiceTypesV1.hashPackage(proposed);
        if (ZkSysServiceTypesV1.hashPackage(expected) != replacement) revert InvalidPackage();

        bytes32 previous = frozenPackageHash;
        _frozenPackage = proposed;
        frozenPackageHash = replacement;
        // Repair changes what is endorsed without giving the producer a new wrapper or deadline.
        emit PackageRepaired(previous, replacement);
    }

    function selectedWrapperIndex() public view returns (uint32) {
        return uint32((uint256(firstWrapperIndex) + currentTurn()) % rosterCount);
    }

    function turnDeadline() external view returns (uint256) {
        return uint256(turnsStartedAt) + (uint256(currentTurn()) + 1) * turnSeconds;
    }

    function packageDigest(AcceptedPackageV1 calldata accepted) external view returns (bytes32) {
        return _hashTypedDataV4(ZkSysServiceTypesV1.hashPackage(accepted));
    }

    function frozenPackage() external view returns (AcceptedPackageV1 memory) {
        return _frozenPackage;
    }

    /// @dev The gate must run native proof verification and this call in the same reverting
    /// transaction. An arbitrary caller can never declare a proof verified through this interface.
    function acceptPackage(
        AcceptedPackageV1 calldata accepted,
        WrapperCandidateV1 calldata candidate,
        bytes32[] calldata candidateProof,
        bytes calldata sequencerSignature,
        bytes calldata wrapperSignature
    ) external onlyGate returns (bytes32 packageHash) {
        uint32 turn = currentTurn();
        if (
            accepted.turn != turn || accepted.proofHash == bytes32(0) || candidate.index != selectedWrapperIndex()
                || candidate.account == address(0) || candidate.operator == address(0)
                || candidate.operator == sequencer || candidate.beneficiary == address(0)
                || accepted.wrapper != candidate.operator || accepted.wrapperBeneficiary != candidate.beneficiary
                || candidateProof.length > 32
                || !MerkleProof.verifyCalldata(candidateProof, rosterRoot, ZkSysServiceTypesV1.wrapperLeaf(candidate))
        ) revert WrongWrapper();

        AcceptedPackageV1 memory normalized = accepted;
        normalized.turn = 0;
        normalized.proofHash = bytes32(0);
        normalized.wrapper = address(0);
        normalized.wrapperBeneficiary = address(0);
        if (ZkSysServiceTypesV1.hashPackage(normalized) != frozenPackageHash) revert InvalidPackage();

        packageHash = ZkSysServiceTypesV1.hashPackage(accepted);
        bytes32 digest = _hashTypedDataV4(packageHash);
        if (
            !sequencer.isValidSignatureNow(digest, sequencerSignature)
                || !candidate.operator.isValidSignatureNow(digest, wrapperSignature)
        ) revert InvalidSignature();

        acceptedParent = packageHash;
        nextBatch = accepted.batchTo + 1;
        ++packageOrdinal;
        packageOpen = false;
        emit PackageAccepted(packageHash, accepted.batchTo, candidate.operator, turn);
    }
}
