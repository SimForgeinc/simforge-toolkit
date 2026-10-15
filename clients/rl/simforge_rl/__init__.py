"""Gymnasium environments and the runtime-protocol client for the SimForge runtime."""

import importlib.metadata

from .env import SimForgeEnv, action_space_for, sensor_space, state_space
from .rewards import EventPenalty, WeightedTerms, native
from .batch import SimForgeBatchVectorEnv
from .runtime import BatchResults, BatchRow, CommandResult, RuntimeBatch, RuntimeClient, RuntimeProtocolError
from .exports import ExportError, export_scenarios, pull_export, read_export
from .scenarios import MapPin, RuntimeSessions, Scenario, ScenarioServers, ScenarioSet, ScenarioSetEnv, bundled_sets
from .server import EnvServer, template_workspace
from .world import Subscription, TruthDelivery, WorldCheckpoint, WorldSession
from .transport import EnvTransport, RuntimeEnvTransport, SocketTransport
from .vector import make_vector_env
from .wire import PROTOCOL, EnvServeError, ProtocolError

__version__ = importlib.metadata.version("simforge-rl")

__all__ = [
    "PROTOCOL",
    "BatchResults",
    "BatchRow",
    "CommandResult",
    "RuntimeBatch",
    "SimForgeBatchVectorEnv",
    "Subscription",
    "TruthDelivery",
    "WorldCheckpoint",
    "WorldSession",
    "EnvServeError",
    "EnvServer",
    "EnvTransport",
    "EventPenalty",
    "ExportError",
    "MapPin",
    "ProtocolError",
    "RuntimeClient",
    "RuntimeEnvTransport",
    "RuntimeProtocolError",
    "RuntimeSessions",
    "Scenario",
    "ScenarioServers",
    "ScenarioSet",
    "ScenarioSetEnv",
    "SimForgeEnv",
    "SocketTransport",
    "WeightedTerms",
    "action_space_for",
    "bundled_sets",
    "export_scenarios",
    "make_vector_env",
    "native",
    "pull_export",
    "read_export",
    "sensor_space",
    "state_space",
    "template_workspace",
]
