<template>
  <Dialog @keydown="handleKeyDown">
    <template #title>
      <div class="title">{{ t('dialog.selectSource') }}</div>
    </template>
    <template #desc>
      <div class="desc">
        {{ t('dialog.selectSourceDesc', { title }) }}
      </div>
    </template>
    <template #body>
      <div class="card-container">
        <template v-for="i in sources">
          <div
            class="card"
            v-if="!i.hidden || showHiddenSources || forcedId === i.id"
            :key="i.id"
            :class="{ active: i.uri === selectedUri }"
            @click="$emit('select', i.uri)"
          >
            <SafeIcon
              :svg-content="i.icon"
              :fallback-component="iconFor(i.uri)"
            />
            <span>{{ i.name }}</span>
          </div>
        </template>
      </div>
    </template>
  </Dialog>
</template>

<script lang="ts" setup>
import { ref, watch } from 'vue';
import Dialog from '../Dialog.vue';
import Cloud from '../Cloud.vue';
import CloudPaid from '../CloudPaid.vue';
import Feedback from '../Feedback.vue';
import SafeIcon from './SafeIcon.vue';
import { t } from '../i18n';
import type { SourceItem } from '../types';

const props = defineProps<{
  title: string;
  sources: SourceItem[];
  selectedUri: string;
  forcedId?: string;
  active: boolean;
}>();

defineEmits<{
  select: [uri: string];
}>();

const commaCount = ref(0);
const showHiddenSources = ref(false);
const commaTimeout = ref(0);

function iconFor(uri: string) {
  if (uri.includes('=beta')) return Feedback;
  if (uri.startsWith('mirrorc://')) return CloudPaid;
  return Cloud;
}

function resetHiddenSources() {
  commaCount.value = 0;
  showHiddenSources.value = false;
  if (commaTimeout.value) {
    clearTimeout(commaTimeout.value);
    commaTimeout.value = 0;
  }
}

function handleKeyDown(event: KeyboardEvent) {
  if (event.key !== ',' && event.code !== 'Comma') {
    return;
  }
  event.preventDefault();
  if (commaTimeout.value) {
    clearTimeout(commaTimeout.value);
  }
  commaCount.value++;
  if (commaCount.value >= 5) {
    showHiddenSources.value = true;
    commaCount.value = 0;
    return;
  }
  commaTimeout.value = setTimeout(() => {
    commaCount.value = 0;
    commaTimeout.value = 0;
  }, 2000);
}

watch(
  () => props.active,
  (active, wasActive) => {
    if (wasActive && !active) {
      resetHiddenSources();
    }
  },
);
</script>

<style scoped>
.title {
  font-size: 25px;
  padding: 2px 10px 6px;
}
.desc {
  font-size: 14px;
  opacity: 0.8;
  padding-left: 10px;
  padding-bottom: 2px;
}
.card {
  padding: 8px 10px;
  font-size: 12px;
  opacity: 0.6;
  border: 1px solid #fff;
  border-radius: 5px;
  width: 74px;
  height: 74px;
  text-align: center;
  display: flex;
  flex-direction: column;
  justify-content: space-evenly;
  align-items: center;
  cursor: pointer;
  transition: all 0.1s ease-in-out;
  &:hover {
    opacity: 1;
  }
  &.active {
    background: rgba(255, 255, 255, 0.1);
    opacity: 1;
  }
}
.card-container {
  padding: 8px 10px;
  display: flex;
  gap: 18px;
  justify-content: center;
  align-items: center;
  height: 150px;
  app-region: no-drag;
}
.card svg {
  width: 40px;
}
</style>
