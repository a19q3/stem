#!/usr/bin/env python3
"""Attribute proposal-to-quorum time from local monotonic trace records."""

import argparse
from collections import defaultdict
import json
from pathlib import Path
import statistics

PROPOSAL = 1
VOTE = 2


def percentiles(values):
    values = sorted(values)
    if not values:
        return {"count": 0}
    return {"count": len(values), "p50_ms": statistics.median(values) / 1e6,
            "p95_ms": values[(len(values)*95+99)//100-1] / 1e6,
            "p99_ms": values[(len(values)*99+99)//100-1] / 1e6}


def offset_bound(samples):
    return [max(remote-replied for _,remote,replied in samples),
            min(remote-requested for requested,remote,_ in samples)]


def clock_bounds(folder):
    file = folder / "clock-samples.json"
    probes = json.loads(file.read_bytes()) if file.exists() else {}
    if not probes.get("pre") or not probes.get("post"):
        return [0,0]
    pre, post = (offset_bound(probes[key]) for key in ("pre","post"))
    if pre[0] > pre[1] or post[0] > post[1]:
        raise ValueError("inconsistent clock probes")
    return [min(pre[0],post[0]), max(pre[1],post[1])]


def load_metrics(report, lo, hi, cutoff):
    start, end, drain = (seconds * 1_000_000 for seconds in (lo, hi, cutoff))
    a, b = (next(row for row in reversed(report["samples"]) if row["at_us"] <= boundary)
            for boundary in (start, end))
    pairs = list(zip(a["statuses"], b["statuses"], strict=True))
    admission_fields = (
        "client_requests", "relayed_requests", "relay_retry_attempts",
        "relay_verification_us", "relay_capacity_rejections", "client_capacity_rejections",
    )
    admission = {
        field: sum(new["admission"][field] - old["admission"][field] for old, new in pairs)
        for field in admission_fields
    }
    traffic = defaultdict(lambda: defaultdict(int))
    for old, new in pairs:
        for left, right in zip(old["wire_traffic_by_kind"], new["wire_traffic_by_kind"], strict=True):
            kind = right["kind"]
            if left["kind"] != kind:
                raise ValueError("unmatched wire counter kinds")
            for field, value in right.items():
                if field != "kind":
                    traffic[kind][field] += value - left[field]
    seen = {item: block["seen_us"] for block in report["blocks"] for item in block["ids"]}
    pending = [0,0,0]
    offered = admitted = 0
    delay = []
    for row in report["deliveries"]:
        write, ack = row["offered_us"], row["acknowledged_us"]
        final = seen.get(row["id"])
        if write is not None and start < write <= end:
            offered += 1
            if ack is not None and final is not None and final <= drain:
                delay.append(max(0,final-write)*1000)
        if ack is not None and start < ack <= end:
            admitted += 1
        if ack is not None:
            for index, boundary in enumerate((start,end,drain)):
                pending[index] += ack <= boundary and (final is None or final > boundary)
    return {
        "writes_in_window":offered, "acknowledgements_in_window":admitted,
        "confirmations_in_window":sum(start<t<=end for t in seen.values()),
        "confirmations_at_cutoff":sum(t<=drain for t in seen.values()),
        "pending_at_start_end_cutoff":pending, "conditional_latency":percentiles(delay),
        "wire_counter_window_us":[a["at_us"],b["at_us"]], "wire_by_kind":traffic,
        "admission_counters":admission,
        "local_timeouts_in_window":sum(y["execution"]["local_timeouts"]-x["execution"]["local_timeouts"] for x,y in pairs),
        "driver_observer_errors":len(report["observation_errors"]),
        "terminal_height":report["agreed_height"], "workload_id":report["workload_id"],
        "both_indexes_active":all(s["cell_node_index"]["active"] and s["live_cell_index"]["active"]
                                  for row in report["samples"] for s in row["statuses"]),
    }


def analyze(folder, report, lo, hi, minimum_bytes):
    epochs = defaultdict(lambda: defaultdict(list))
    headers = {}
    offset = clock_bounds(folder)
    for path in sorted(folder.glob("trace-*.jsonl")):
        with path.open() as file:
            first = json.loads(file.readline())
            if first["schema"] != 1 or first["replica"] in headers:
                raise ValueError("unexpected trace schema or duplicate replica")
            node = first["replica"]
            headers[node] = first
            for line in file:
                event = json.loads(line)
                key = (event["epoch"],event["view"],event["parent_view"],event["header"])
                epochs[key][node].append(event)
    if set(headers) != {0,1,2,3}:
        raise ValueError("four complete replica traces are required")

    clock = report["driver_clock"]["start"]
    origin = clock["unix_us"]*1000-(clock["before_us"]+clock["after_us"])*500
    lower, upper = origin+lo*1_000_000_000, origin+hi*1_000_000_000
    times = defaultdict(list)
    counts = defaultdict(int)

    def stamp(node, event):
        anchor=headers[node]
        adjustment=offset if node >= 2 else [0,0]
        instant=event["t_ns"]+anchor["unix_ns"]
        return [instant-anchor["clock_bracket_ns"]-adjustment[1], instant-adjustment[0]]

    def select(events, stage, *, kind=None, peer=None):
        return [event for event in events if event["stage"]==stage
                and (kind is None or event["kind"]==kind)
                and (peer is None or event["peer"]==peer)]

    def first(events, stage, *, kind=None, peer=None):
        entries=select(events,stage,kind=kind,peer=peer)
        return min(entries,key=lambda e:e["t_ns"],default=None)

    def difference(phase, start, end):
        if start is not None and end is not None and start["t_ns"]<=end["t_ns"]:
            times[phase].append(end["t_ns"]-start["t_ns"])

    def unique_before(sender, sender_rows, receiver, arrival, kind):
        # Wire retries carry the same Point. Use only deliveries for which a
        # single write could have caused the first complete receipt.
        limit=stamp(receiver,arrival)[1]
        writes=select(sender_rows,"tx_write_begin",kind=kind,peer=receiver)
        possible=[w for w in writes if stamp(sender,w)[0]<=limit]
        return possible[0] if len(possible)==1 else None

    for broadcasts in epochs.values():
        proposers=[n for n,entries in broadcasts.items() if first(entries,"proposal_prepared")]
        if len(proposers)!=1:
            continue
        leader=proposers[0]
        own=broadcasts[leader]
        prepared=first(own,"proposal_prepared")
        if not lower<=stamp(leader,prepared)[0] <= upper:
            continue
        outbound=select(own,"tx_kernel_write_done",kind=PROPOSAL)
        if not any(event["bytes"]>=minimum_bytes for event in outbound):
            continue
        counts["scored_proposals"]+=1
        counts["proposal_copies_written"]+=len(outbound)
        peers = set()
        for frame in sorted(outbound, key=lambda event:event["t_ns"]):
            peer = frame["peer"]
            cross_split = leader//2 != peer//2
            counts["proposal_bytes_written"]+=frame["bytes"]
            counts["proposal_cross_split_bytes_written"]+=frame["bytes"]*cross_split
            if peer in peers:
                counts["proposal_extra_copies_written"]+=1
                counts["proposal_extra_bytes_written"]+=frame["bytes"]
                counts["proposal_cross_split_extra_bytes_written"]+=frame["bytes"]*cross_split
            peers.add(peer)
        for node, rows in broadcasts.items():
            if node != leader:
                difference("any_follower_candidate_work",
                           first(rows,"candidate_work_begin"),first(rows,"candidate_work_end"))
        durable=first(own,"proposal_durable")
        qc=first(own,"quorum_durable")
        difference("leader_prepared_to_proposal_durable",prepared,durable)
        difference("leader_proposal_durable_to_quorum_durable",durable,qc)
        if qc:
            counts["leader_quorum_durable"]+=1
        assembled=first(own,"quorum_assembled")
        if assembled is None:
            continue
        counts["leader_quorum_assembled"]+=1
        voter=assembled["peer"]
        if voter==leader:
            continue
        remote=broadcasts.get(voter,())
        proposal_read=first(remote,"rx_read_complete",kind=PROPOSAL,peer=leader)
        vote_read=first(own,"rx_read_complete",kind=VOTE,peer=voter)
        if proposal_read is None or vote_read is None:
            counts["missing_closing_receipt"]+=1
            continue
        proposal_write=unique_before(leader,own,voter,proposal_read,PROPOSAL)
        vote_write=unique_before(voter,remote,leader,vote_read,VOTE)
        if proposal_write is None or vote_write is None:
            counts["ambiguous_closing_wire_copy"]+=1
            continue
        round_trip=vote_read["t_ns"]-proposal_write["t_ns"]
        follower=vote_write["t_ns"]-proposal_read["t_ns"]
        if min(round_trip,follower)<0 or round_trip<follower:
            counts["inconsistent_closing_wire_clocks"]+=1
            continue
        counts["fully_attributed_closing_vote"]+=1
        counts["closing_vote_cross_host"]+=leader//2 != voter//2
        times["closing_round_trip"].append(round_trip)
        times["closing_follower_total"].append(follower)
        # This residual uses only a duration from each replica and therefore
        # needs no shared wall-clock offset. It includes kernel transfer work.
        times["closing_wire_round_trip_residual"].append(round_trip-follower)
        difference("follower_read_to_inbox",proposal_read,first(
            remote,"rx_inbox_enqueue",kind=PROPOSAL,peer=leader))
        difference("follower_handler_to_candidate_work",first(
            remote,"rx_handler_begin",kind=PROPOSAL,peer=leader),first(remote,"candidate_work_begin"))
        difference("follower_candidate_to_durable_vote",first(
            remote,"candidate_work_end"),first(remote,"vote_durable"))
        traces = (
            ("leader_outbound_queue",own,"tx_queued",own,"tx_dequeued",PROPOSAL,voter),
            ("follower_inbound_queue",remote,"rx_inbox_enqueue",remote,"rx_handler_begin",PROPOSAL,leader),
            ("collector_inbound_queue",own,"rx_inbox_enqueue",own,"rx_handler_begin",VOTE,voter),
            ("follower_candidate_work",remote,"candidate_work_begin",remote,"candidate_work_end",None,None),
            ("follower_persist_vote",remote,"vote_ready",remote,"vote_durable",None,None),
            ("follower_vote_send_queue",remote,"tx_queued",remote,"tx_dequeued",VOTE,leader),
        )
        for stage,left,begin,right,end,kind,peer in traces:
            difference(stage,first(left,begin,kind=kind,peer=peer),first(right,end,kind=kind,peer=peer))
        difference("collector_vote_read_to_quorum_assembly",vote_read,assembled)
        difference("collector_quorum_assembly_to_durable",assembled,qc)
        if leader//2 != voter//2:
            sent, received = stamp(leader,proposal_write), stamp(voter,proposal_read)
            times["closing_cross_host_proposal_lower"].append(max(0,received[0]-sent[1]))
            times["closing_cross_host_proposal_upper"].append(max(0,received[1]-sent[0]))
    return {"clock_offset_ns":offset, "window_seconds":[lo,hi], "minimum_proposal_bytes":minimum_bytes,
            "counts":counts, "latency":{phase:percentiles(v) for phase,v in sorted(times.items())}}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--traces",type=Path,required=True)
    parser.add_argument("--report",type=Path,required=True)
    parser.add_argument("--start",type=int,default=20)
    parser.add_argument("--stop",type=int,default=60)
    parser.add_argument("--cutoff",type=int,default=95)
    parser.add_argument("--minimum-proposal-bytes",type=int,default=1024)
    args=parser.parse_args()
    if not 0 <= args.start < args.stop < args.cutoff:
        parser.error("invalid scoring window")
    report=json.loads(args.report.read_bytes())
    evidence = analyze(args.traces,report,args.start,args.stop,args.minimum_proposal_bytes)
    evidence["load"] = load_metrics(report,args.start,args.stop,args.cutoff)
    print(json.dumps(evidence,indent=2,sort_keys=True))


if __name__=="__main__":
    main()
