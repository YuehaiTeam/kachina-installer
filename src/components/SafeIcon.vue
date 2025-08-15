<template>
  <div 
    v-if="sanitizedSvg" 
    class="safe-icon"
    v-html="sanitizedSvg"
  />
  <component v-else :is="fallbackComponent" />
</template>

<script setup lang="ts">
import { computed } from 'vue';
import { sanitizeSvg } from '../utils/svgSanitizer';

interface Props {
  svgContent?: string;
  fallbackComponent: any;
}

const props = defineProps<Props>();

const sanitizedSvg = computed(() => {
  return props.svgContent ? sanitizeSvg(props.svgContent) : null;
});
</script>

<style scoped>
.safe-icon :deep(svg) {
  fill: currentColor;
  box-sizing: border-box;
}
</style>