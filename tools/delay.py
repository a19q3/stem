"""Bounded loopback TCP forwarding with one-way delay and link interruption."""

import asyncio
import concurrent.futures
import threading


class NetworkDelay:
    def __init__(self, routes, delay_ms):
        self.routes = routes
        self.delay = delay_ms / 1000
        self.ready = concurrent.futures.Future()
        self.loop = None
        self.done = None
        self.thread = None
        self.blocked = set()
        self.active = {}

    def __enter__(self):
        if self.routes:
            self.thread = threading.Thread(target=self.run, name="p2p-delay", daemon=True)
            self.thread.start()
            self.ready.result(timeout=8)
        return self

    def __exit__(self, *exc):
        if self.thread:
            self.loop.call_soon_threadsafe(self.done.set)
            self.thread.join(timeout=8)

    def block(self, ports):
        """Set the disabled routes and close any streams crossing them."""
        future = asyncio.run_coroutine_threadsafe(self.partition(set(ports)), self.loop)
        future.result(timeout=8)

    async def partition(self, ports):
        self.blocked = ports
        closing = [task for port in ports for task in self.active.get(port, ())]
        for task in closing:
            task.cancel()
        await asyncio.gather(*closing, return_exceptions=True)

    def run(self):
        try:
            asyncio.run(self.serve())
        except Exception as error:
            if not self.ready.done():
                self.ready.set_exception(error)
            else:
                self.loop.call_exception_handler({"message": "Delayed P2P relay failed", "exception": error})

    async def serve(self):
        self.loop = asyncio.get_running_loop()
        self.done = asyncio.Event()
        listeners = []
        try:
            for port, upstream in self.routes:
                async def connection(reader, writer, route=port, target=upstream):
                    await self.connection(reader, writer, route, target)

                listeners.append(await asyncio.start_server(connection, "127.0.0.1", port))
            self.ready.set_result(None)
            await self.done.wait()
        finally:
            for listener in listeners:
                listener.close()
            await asyncio.gather(*(listener.wait_closed() for listener in listeners))

    async def connection(self, reader, writer, route, target):
        task = asyncio.current_task()
        active = self.active.setdefault(route, set())
        try:
            if route not in self.blocked:
                active.add(task)
                await self.relay(reader, writer, target)
        finally:
            active.discard(task)
            writer.close()
            await asyncio.gather(writer.wait_closed(), return_exceptions=True)

    async def relay(self, incoming, outgoing, upstream):
        try:
            upstream_in, upstream_out = await asyncio.open_connection("127.0.0.1", upstream)
        except OSError:
            return

        async def forward(source, target):
            queue = asyncio.Queue(maxsize=16)

            async def receive():
                try:
                    while chunk := await source.read(64 * 1024):
                        await queue.put((self.loop.time() + self.delay, chunk))
                finally:
                    # Wake the writer on EOF or a read error. A cancellation
                    # comes from the outer connection cleanup.
                    if not asyncio.current_task().cancelling():
                        await queue.put((0, b""))

            async def send():
                while True:
                    when, chunk = await queue.get()
                    if not chunk:
                        return
                    await asyncio.sleep(max(0, when - self.loop.time()))
                    target.write(chunk)
                    await target.drain()

            reader = asyncio.create_task(receive())
            try:
                await send()
            finally:
                reader.cancel()
                values = await asyncio.gather(reader, return_exceptions=True)
                if isinstance(values[0], Exception):
                    raise values[0]

        tasks = [asyncio.create_task(forward(incoming, upstream_out)),
                 asyncio.create_task(forward(upstream_in, outgoing))]
        try:
            await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        finally:
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            upstream_out.close()
            await asyncio.gather(upstream_out.wait_closed(), return_exceptions=True)
